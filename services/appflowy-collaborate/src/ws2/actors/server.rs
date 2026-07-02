use super::session::{WsInput, WsSession};
use super::workspace::{Terminate, Workspace};
use crate::collab::collab_manager::CollabManager;
use crate::collab::snapshot_scheduler::SnapshotScheduler;
use crate::ws2::{BulkPermissionUpdate, PermissionUpdate};
use actix::{Actor, Addr, Arbiter, AsyncContext, Handler, Recipient};
use app_error::AppError;
use appflowy_proto::{ObjectId, Rid, ServerMessage, WorkspaceId};
use collab::core::origin::CollabOrigin;
use collab_entity::CollabType;
use collab_folder::Folder;
use database::collab::AppResult;
use std::collections::HashMap;
use std::fmt::Display;
use std::time::Duration;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use tracing::info;
use yrs::block::ClientID;

/// 【共享协作路由 2026-07-02】对象的路由结果。
/// - `To(ws)`:collab 已存在,消息一律桥接到其真实归属 workspace 的 actor(归属不可变,
///   缓存永久有效);
/// - `Passthrough`:collab 尚不存在(新建场景),按消息自带的连接 workspace 投递。
///   带过期时间:文档落库后若被分享给他人,需要重新解析出真实归属,否则新被分享者的
///   消息会按旧缓存透传回其自己的 workspace(复现路由 bug)。
#[derive(Clone, Copy, PartialEq, Eq)]
enum ObjectRoute {
  Passthrough(std::time::Instant),
  To(WorkspaceId),
}

/// Passthrough 路由的有效期:超过后重新解析对象归属。
const PASSTHROUGH_ROUTE_TTL: Duration = Duration::from_secs(60);

/// 路由缓存容量上限(collab 归属不可变,仅防 HashMap 无界增长)。
const OBJECT_ROUTE_CACHE_LIMIT: usize = 200_000;

pub struct WsServer {
  manager: Arc<CollabManager>,
  snapshot_scheduler: SnapshotScheduler,
  workspaces: HashMap<WorkspaceId, Addr<Workspace>>,
  arbiter_pool: ArbiterPool,
  /// 【共享协作路由】pg 连接池:解析 object → 真实 workspace 归属。
  pg_pool: sqlx::PgPool,
  /// object → 路由结果缓存(归属不可变,一次解析长期有效)。
  object_routes: HashMap<ObjectId, ObjectRoute>,
  /// 解析进行中的 object 的待投递消息队列(解析完成后按序投递)。
  pending_route_inputs: HashMap<ObjectId, Vec<WsInput>>,
  /// 各会话以 guest 身份加入过的外部 workspace(Leave 时统一清理)。
  guest_rooms: HashMap<ClientID, std::collections::HashSet<WorkspaceId>>,
}

impl WsServer {
  pub fn new(manager: Arc<CollabManager>, pg_pool: sqlx::PgPool) -> Self {
    let snapshot_scheduler = SnapshotScheduler::new(manager.clone());
    let arbiter_pool = ArbiterPool::default();
    Self {
      manager,
      snapshot_scheduler,
      workspaces: HashMap::new(),
      arbiter_pool,
      pg_pool,
      object_routes: HashMap::new(),
      pending_route_inputs: HashMap::new(),
      guest_rooms: HashMap::new(),
    }
  }

  fn init_workspace(
    server: Recipient<Terminate>,
    workspace_id: WorkspaceId,
    manager: Arc<CollabManager>,
    snapshot_scheduler: SnapshotScheduler,
    pool: &ArbiterPool,
  ) -> Addr<Workspace> {
    let arbiter = pool.next();
    Workspace::start_in_arbiter(&arbiter.handle(), move |_ctx| {
      Workspace::new(server, workspace_id, manager, snapshot_scheduler)
    })
  }

  fn get_or_init_workspace(
    &mut self,
    workspace_id: WorkspaceId,
    ctx: &mut actix::Context<Self>,
  ) -> &Addr<Workspace> {
    let server = ctx.address().recipient();
    self.workspaces.entry(workspace_id).or_insert_with(|| {
      Self::init_workspace(
        server,
        workspace_id,
        self.manager.clone(),
        self.snapshot_scheduler.clone(),
        &self.arbiter_pool,
      )
    })
  }

  /// 按已解析路由投递消息。跨 workspace 时先确保该会话以 guest 身份加入目标房间
  /// (同一 actor 邮箱内 Join 先于 WsInput 处理,顺序有保证),再改写 workspace_id 投递,
  /// 使 Manifest/Update/Awareness 都作用于文档真实归属的房间与数据流。
  fn deliver_routed(
    &mut self,
    mut msg: WsInput,
    target_ws: WorkspaceId,
    ctx: &mut actix::Context<Self>,
  ) {
    if msg.workspace_id != target_ws {
      let joined = self.guest_rooms.entry(msg.client_id).or_default();
      if joined.insert(target_ws) {
        let uid = msg.sender.client_user_id().unwrap_or(0);
        info!(
          "[共享协作路由] session {} (uid {}) guest 加入 workspace {} (对象 {} 的真实归属)",
          msg.client_id, uid, target_ws, msg.object_id
        );
        let join = Join {
          uid,
          session_id: msg.client_id,
          collab_origin: msg.sender.clone(),
          // guest 加入不做历史 collab 重放(None),仅注册以接收实时广播;
          // 文档内容补齐由客户端 Manifest(state_vector diff)完成。
          last_message_id: None,
          addr: msg.addr.clone(),
          workspace_id: target_ws,
        };
        self.get_or_init_workspace(target_ws, ctx).do_send(join);
      }
      msg.workspace_id = target_ws;
    }
    self.get_or_init_workspace(target_ws, ctx).do_send(msg);
  }
}

/// 【共享协作路由】对象归属解析完成的回执。
#[derive(actix::Message)]
#[rtype(result = "()")]
struct ObjectRouteResolved {
  object_id: ObjectId,
  workspace_id: Option<WorkspaceId>,
}

impl Actor for WsServer {
  type Context = actix::Context<Self>;
}

impl Handler<Join> for WsServer {
  type Result = ();

  fn handle(&mut self, msg: Join, ctx: &mut Self::Context) -> Self::Result {
    let server = ctx.address().recipient();
    let workspace = self.workspaces.entry(msg.workspace_id).or_insert_with(|| {
      Self::init_workspace(
        server,
        msg.workspace_id,
        self.manager.clone(),
        self.snapshot_scheduler.clone(),
        &self.arbiter_pool,
      )
    });
    info!("{} joined", msg);
    workspace.do_send(msg);
  }
}

impl Handler<Leave> for WsServer {
  type Result = ();

  fn handle(&mut self, msg: Leave, _ctx: &mut Self::Context) -> Self::Result {
    // 【共享协作路由】会话离开时,同时从其 guest 加入过的外部 workspace 房间注销,
    // 避免 workspace actor 持有悬空的会话句柄。
    if let Some(guest_workspaces) = self.guest_rooms.remove(&msg.session_id) {
      for guest_ws in guest_workspaces {
        if guest_ws != msg.workspace_id {
          if let Some(workspace) = self.workspaces.get(&guest_ws) {
            workspace.do_send(Leave {
              session_id: msg.session_id,
              workspace_id: guest_ws,
            });
          }
        }
      }
    }
    if let Some(workspace) = self.workspaces.get(&msg.workspace_id) {
      workspace.do_send(msg);
    }
  }
}

impl Handler<WsInput> for WsServer {
  type Result = ();

  fn handle(&mut self, msg: WsInput, ctx: &mut Self::Context) -> Self::Result {
    // 【共享协作路由 2026-07-02】按对象真实归属路由,而非无条件按连接 workspace。
    // 背景:被分享者打开共享文档时,客户端只有一条连到"自己当前 workspace"的 WS,
    // 其 Manifest/Update 此前被投进自己 workspace 的房间与数据流——拥有者在文档
    // 真实 workspace 的房间里永远收不到,表现为"被分享者的编辑拥有者看不到"。
    let route = match self.object_routes.get(&msg.object_id).copied() {
      // Passthrough 过期后按未解析处理(文档可能已落库并被分享,需重查真实归属)。
      Some(ObjectRoute::Passthrough(resolved_at))
        if resolved_at.elapsed() >= PASSTHROUGH_ROUTE_TTL =>
      {
        self.object_routes.remove(&msg.object_id);
        None
      },
      other => other,
    };
    match route {
      Some(ObjectRoute::Passthrough(_)) => {
        let target = msg.workspace_id;
        self.deliver_routed(msg, target, ctx);
      },
      Some(ObjectRoute::To(real_ws)) => {
        self.deliver_routed(msg, real_ws, ctx);
      },
      None => {
        use std::collections::hash_map::Entry;
        match self.pending_route_inputs.entry(msg.object_id) {
          Entry::Occupied(mut pending) => {
            // 解析已在进行,排队等结果(保序)。
            pending.get_mut().push(msg);
          },
          Entry::Vacant(slot) => {
            let object_id = msg.object_id;
            slot.insert(vec![msg]);
            let pool = self.pg_pool.clone();
            let addr = ctx.address();
            actix::spawn(async move {
              let workspace_id =
                database::collab::select_collab_workspace_id(&pool, &object_id)
                  .await
                  .unwrap_or_else(|err| {
                    tracing::warn!(
                      "[共享协作路由] 解析对象 {} 归属失败(按连接 workspace 透传): {}",
                      object_id,
                      err
                    );
                    None
                  });
              addr.do_send(ObjectRouteResolved {
                object_id,
                workspace_id,
              });
            });
          },
        }
      },
    }
  }
}

impl Handler<ObjectRouteResolved> for WsServer {
  type Result = ();

  fn handle(&mut self, msg: ObjectRouteResolved, ctx: &mut Self::Context) -> Self::Result {
    let route = match msg.workspace_id {
      Some(ws) => ObjectRoute::To(ws),
      // collab 尚不存在(新建)或解析失败:按连接 workspace 透传(原有行为),带 TTL。
      None => ObjectRoute::Passthrough(std::time::Instant::now()),
    };
    // To(ws) 归属不可变,缓存长期有效;仅在极端规模下整体重置防止无界增长。
    if self.object_routes.len() >= OBJECT_ROUTE_CACHE_LIMIT {
      self.object_routes.clear();
    }
    self.object_routes.insert(msg.object_id, route);
    if let Some(pending) = self.pending_route_inputs.remove(&msg.object_id) {
      for queued in pending {
        match route {
          ObjectRoute::Passthrough(_) => {
            let target = queued.workspace_id;
            self.deliver_routed(queued, target, ctx);
          },
          ObjectRoute::To(real_ws) => self.deliver_routed(queued, real_ws, ctx),
        }
      }
    }
  }
}

impl Handler<Terminate> for WsServer {
  type Result = ();

  fn handle(&mut self, msg: Terminate, _ctx: &mut Self::Context) -> Self::Result {
    self.workspaces.remove(&msg.workspace_id);
    tracing::debug!("workspace {} closed", msg.workspace_id);
  }
}

impl Handler<PublishUpdate> for WsServer {
  type Result = ();

  fn handle(&mut self, msg: PublishUpdate, ctx: &mut Self::Context) -> Self::Result {
    let server = ctx.address().recipient();
    let workspace = self.workspaces.entry(msg.workspace_id).or_insert_with(|| {
      Self::init_workspace(
        server,
        msg.workspace_id,
        self.manager.clone(),
        self.snapshot_scheduler.clone(),
        &self.arbiter_pool,
      )
    });
    workspace.do_send(msg);
  }
}

impl Handler<UpdateUserPermissions> for WsServer {
  type Result = ();

  fn handle(&mut self, msg: UpdateUserPermissions, _ctx: &mut Self::Context) -> Self::Result {
    if let Some(workspace) = self.workspaces.get(&msg.workspace_id) {
      workspace.do_send(msg);
    }
  }
}

impl Handler<RefreshWorkspaceUserPermissions> for WsServer {
  type Result = ();

  fn handle(
    &mut self,
    msg: RefreshWorkspaceUserPermissions,
    _ctx: &mut Self::Context,
  ) -> Self::Result {
    if let Some(workspace) = self.workspaces.get(&msg.workspace_id) {
      workspace.do_send(msg);
    }
  }
}

impl Handler<BroadcastPermissionChanges> for WsServer {
  type Result = ();

  fn handle(&mut self, msg: BroadcastPermissionChanges, _ctx: &mut Self::Context) -> Self::Result {
    for workspace in self.workspaces.values() {
      workspace.do_send(BroadcastPermissionChanges {
        changes: msg.changes.clone(),
        exclude_uid: msg.exclude_uid,
      });
    }
  }
}

impl Handler<WorkspaceFolder> for WsServer {
  type Result = ();

  fn handle(&mut self, msg: WorkspaceFolder, ctx: &mut Self::Context) -> Self::Result {
    let server = ctx.address().recipient();
    let workspace = self.workspaces.entry(msg.workspace_id).or_insert_with(|| {
      Self::init_workspace(
        server,
        msg.workspace_id,
        self.manager.clone(),
        self.snapshot_scheduler.clone(),
        &self.arbiter_pool,
      )
    });
    workspace.do_send(msg);
  }
}

#[derive(actix::Message)]
#[rtype(result = "()")]
pub struct Join {
  pub uid: i64,
  /// Current client session identifier.
  pub session_id: ClientID,
  pub collab_origin: CollabOrigin,
  pub last_message_id: Option<Rid>,
  /// Actix WebSocket session actor address.
  pub addr: Addr<WsSession>,
  /// Workspace to join.
  pub workspace_id: WorkspaceId,
}

impl Display for Join {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(
      f,
      "Join(uid: {}, session_id: {}, workspace_id: {})",
      self.uid, self.session_id, self.workspace_id
    )
  }
}

#[derive(actix::Message)]
#[rtype(result = "()")]
pub struct Leave {
  /// Current client session identifier.
  pub session_id: ClientID,
  /// Workspace to leave.
  pub workspace_id: WorkspaceId,
}

#[derive(actix::Message)]
#[rtype(result = "()")]
pub struct WsOutput {
  pub message: ServerMessage,
}

#[derive(actix::Message)]
#[rtype(result = "()")]
pub struct UpdateUserPermissions {
  pub workspace_id: WorkspaceId,
  pub uid: i64,
  pub updates: Vec<PermissionUpdate>,
}

#[derive(actix::Message)]
#[rtype(result = "()")]
pub struct RefreshWorkspaceUserPermissions {
  pub workspace_id: WorkspaceId,
  pub uid: i64,
}

#[derive(actix::Message)]
#[rtype(result = "()")]
pub struct BroadcastPermissionChanges {
  pub changes: BulkPermissionUpdate,
  pub exclude_uid: Option<i64>, // Don't send to the user who made the change
}

#[derive(actix::Message)]
#[rtype(result = "()")]
pub struct PublishUpdate {
  pub workspace_id: WorkspaceId,
  pub object_id: ObjectId,
  pub collab_type: CollabType,
  pub sender: CollabOrigin,
  pub update_v1: Vec<u8>,
  pub ack: tokio::sync::oneshot::Sender<anyhow::Result<Rid>>,
}

#[async_trait::async_trait]
pub trait CollabUpdatePublisher {
  async fn publish_update(
    &self,
    workspace_id: WorkspaceId,
    object_id: ObjectId,
    collab_type: CollabType,
    sender: &CollabOrigin,
    update_v1: Vec<u8>,
  ) -> anyhow::Result<Rid>;
}

#[async_trait::async_trait]
impl CollabUpdatePublisher for Addr<WsServer> {
  async fn publish_update(
    &self,
    workspace_id: WorkspaceId,
    object_id: ObjectId,
    collab_type: CollabType,
    sender: &CollabOrigin,
    update_v1: Vec<u8>,
  ) -> anyhow::Result<Rid> {
    let (ack, rx) = tokio::sync::oneshot::channel();
    self.do_send(PublishUpdate {
      workspace_id,
      object_id,
      collab_type,
      sender: sender.clone(),
      update_v1,
      ack,
    });
    rx.await?
  }
}

#[derive(actix::Message)]
#[rtype(result = "()")]
pub struct WorkspaceFolder {
  pub workspace_id: WorkspaceId,
  pub ack: tokio::sync::oneshot::Sender<AppResult<Folder>>,
}

#[async_trait::async_trait]
pub trait WorkspaceCollabInstanceCache {
  async fn get_folder(&self, workspace_id: WorkspaceId) -> AppResult<Folder>;
}

#[async_trait::async_trait]
impl WorkspaceCollabInstanceCache for Addr<WsServer> {
  async fn get_folder(&self, workspace_id: WorkspaceId) -> AppResult<Folder> {
    let (ack, rx) = tokio::sync::oneshot::channel();
    self.do_send(WorkspaceFolder { workspace_id, ack });
    rx.await.map_err(|err| AppError::Internal(err.into()))?
  }
}

pub struct ArbiterPool {
  arbiters: Vec<Arbiter>,
  next: AtomicU64,
}

impl Default for ArbiterPool {
  fn default() -> Self {
    let parallelism = match std::thread::available_parallelism() {
      Ok(max_cpus) => max_cpus.get(),
      Err(_) => 4, // Fallback to a default value if unable to determine
    };
    Self::new(parallelism)
  }
}

impl ArbiterPool {
  pub fn new(size: usize) -> Self {
    tracing::info!("creating arbiter pool on {} threads", size);
    let mut arbiters = Vec::with_capacity(size);
    for _ in 0..size {
      arbiters.push(Arbiter::new());
    }
    Self {
      arbiters,
      next: AtomicU64::new(0),
    }
  }

  pub fn next(&self) -> &Arbiter {
    let index =
      self.next.fetch_add(1, std::sync::atomic::Ordering::SeqCst) % (self.arbiters.len() as u64);
    &self.arbiters[index as usize]
  }
}
