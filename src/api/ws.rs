use std::borrow::Cow;
use std::collections::HashMap;
use std::time::Duration;

use crate::biz::authentication::jwt::{authorization_from_token, UserUuid};
use crate::biz::notification::ops::{get_pending_notifications, mark_notifications_processed};
use crate::state::AppState;
use actix::Addr;
use actix_http::header::AUTHORIZATION;
use actix_web::web::{Data, Path, Payload};
use actix_web::{get, web, HttpRequest, HttpResponse, Result, Scope};
use actix_web_actors::ws;
use app_error::AppError;
use appflowy_collaborate::actix_ws::client::rt_client::RealtimeClient;
use appflowy_collaborate::actix_ws::server::RealtimeServerActor;
use appflowy_collaborate::ws2::{SessionInfo, WsSession};
use appflowy_proto::{ServerMessage, WorkspaceNotification};
use collab_rt_entity::user::{AFSystemNotification, AFUserChange, RealtimeUser, UserMessage};
use collab_rt_entity::{max_sync_message_size, RealtimeMessage};
use collab_stream::model::MessageId;
use secrecy::Secret;
use semver::Version;
use shared_entity::response::AppResponseError;
use tokio::sync::mpsc;
use tokio::sync::mpsc::Sender;
use tracing::{debug, error, instrument, trace};
use uuid::Uuid;

pub fn ws_scope() -> Scope {
  web::scope("/ws")
    //.service(establish_ws_connection)
    .service(web::resource("/v1").route(web::get().to(establish_ws_connection_v1)))
    .service(web::resource("/v2/{workspace_id}").route(web::get().to(establish_ws_connection_v2)))
}
const MAX_FRAME_SIZE: usize = 65_536; // 64 KiB

pub type RealtimeServerAddr = Addr<RealtimeServerActor>;

/// This function will not be used after the 0.5.0 of the client.
#[instrument(skip_all, err)]
#[get("/{token}/{device_id}")]
pub async fn establish_ws_connection(
  request: HttpRequest,
  payload: Payload,
  path: Path<(String, String)>,
  state: Data<AppState>,
  jwt_secret: Data<Secret<String>>,
  server: Data<RealtimeServerAddr>,
) -> Result<HttpResponse> {
  let (access_token, device_id) = path.into_inner();
  let client_version = Version::new(0, 5, 0);
  let connect_at = chrono::Utc::now().timestamp();
  start_connect(
    &request,
    payload,
    &state,
    &jwt_secret,
    server,
    access_token,
    device_id,
    client_version,
    connect_at,
  )
  .await
}

#[instrument(skip_all, err)]
pub async fn establish_ws_connection_v1(
  request: HttpRequest,
  payload: Payload,
  state: Data<AppState>,
  jwt_secret: Data<Secret<String>>,
  server: Data<RealtimeServerAddr>,
  web::Query(query_params): web::Query<HashMap<String, String>>,
) -> Result<HttpResponse> {
  // Try to parse the connect info from the request body
  // If it fails, try to parse it from the query params
  let ConnectInfo {
    access_token,
    client_version,
    device_id,
    connect_at,
  } = match ConnectInfo::parse_from(&request) {
    Ok(info) => info,
    Err(_) => {
      trace!("Failed to parse connect info from request body. Trying to parse from query params.");
      ConnectInfo::parse_from(&query_params)?
    },
  };

  if client_version < state.config.websocket.min_client_version {
    return Err(AppError::Connect("Client version is too low".to_string()).into());
  }

  start_connect(
    &request,
    payload,
    &state,
    &jwt_secret,
    server,
    access_token,
    device_id,
    client_version,
    connect_at,
  )
  .await
}

#[instrument(skip_all, err)]
pub async fn establish_ws_connection_v2(
  request: HttpRequest,
  payload: Payload,
  path: Path<Uuid>,
  state: Data<AppState>,
  jwt_secret: Data<Secret<String>>,
) -> Result<HttpResponse> {
  let workspace_id = path.into_inner();
  let ws_server = state.ws_server.clone();
  let params = WsConnectionV2Params::parse(&request)?;
  let auth = authorization_from_token(params.access_token.as_str(), &jwt_secret)?;
  let user_uuid = UserUuid::from_auth(auth)?;
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  let info = SessionInfo::new(
    params.client_id,
    uid,
    params.device_id,
    params.last_message_id,
  );
  tracing::debug!(
    "accepting new session {} (client id: {}) for workspace: {}",
    info.collab_origin(),
    params.client_id,
    workspace_id
  );

  let (tx, rx) = mpsc::channel(10);

  // 订阅用户资料变更通知
  let tx_user = tx.clone();
  let mut user_change_recv = state.pg_listeners.subscribe_user_change(uid);
  actix::spawn(async move {
    while let Some(notification) = user_change_recv.recv().await {
      if let Some(user) = notification.payload {
        let _ = tx_user
          .send(ServerMessage::Notification {
            notification: WorkspaceNotification::UserProfileChange {
              uid: user.uid,
              name: user.name,
              email: user.email,
            },
          })
          .await;
      }
    }
  });

  // 订阅系统通知（含补发离线期间积压的未处理通知）
  let tx_system = tx.clone();
  let pg_pool_notif = state.pg_pool.clone();
  let mut system_notification_recv = state.pg_listeners.subscribe_system_notification(uid);
  actix::spawn(async move {
    // 先补发离线期间积压的未处理通知
    match get_pending_notifications(&pg_pool_notif, uid).await {
      Ok(pending) if !pending.is_empty() => {
        debug!(
          "v2: delivering {} pending notifications to uid={} on connect",
          pending.len(),
          uid
        );
        let mut delivered_ids = Vec::with_capacity(pending.len());
        for notification in pending {
          let msg = ServerMessage::Notification {
            notification: WorkspaceNotification::SystemNotification {
              id: notification.id.to_string(),
              workspace_id: notification
                .workspace_id
                .map(|id| id.to_string())
                .unwrap_or_default(),
              notification_type: notification.notification_type,
              title: extract_title_from_payload(&notification.payload),
              message: extract_message_from_payload(&notification.payload),
              payload_json: notification.payload.to_string(),
              created_at: notification.created_at.timestamp(),
              recipient_uid: notification.recipient_uid.unwrap_or(0),
            },
          };
          if tx_system.send(msg).await.is_err() {
            return;
          }
          delivered_ids.push(notification.id);
        }
        if let Err(e) = mark_notifications_processed(&pg_pool_notif, &delivered_ids).await {
          error!("v2: failed to mark pending notifications as processed for uid={}: {:?}", uid, e);
        }
      },
      Ok(_) => {},
      Err(e) => error!("v2: failed to fetch pending notifications for uid={}: {:?}", uid, e),
    }

    // 持续监听新到达的通知
    while let Some(notification) = system_notification_recv.recv().await {
      debug!(
        "[ws_v2] Pushing system notification to uid={}: type={}, id={}",
        uid,
        notification.notification_type,
        notification.id
      );
      if let Err(e) = mark_notifications_processed(&pg_pool_notif, &[notification.id]).await {
        error!("v2: failed to mark notification as processed: {:?}", e);
      }
      let _ = tx_system
        .send(ServerMessage::Notification {
          notification: WorkspaceNotification::SystemNotification {
            id: notification.id.to_string(),
            workspace_id: notification
              .workspace_id
              .map(|id| id.to_string())
              .unwrap_or_default(),
            notification_type: notification.notification_type,
            title: extract_title_from_payload(&notification.payload),
            message: extract_message_from_payload(&notification.payload),
            payload_json: notification.payload.to_string(),
            created_at: notification.created_at.timestamp(),
            recipient_uid: notification.recipient_uid.unwrap_or(0),
          },
        })
        .await;
    }
  });

  ws::WsResponseBuilder::new(
    WsSession::new(workspace_id, info, ws_server, rx),
    &request,
    payload,
  )
  .frame_size(max_sync_message_size())
  .start()
}

/// 从通知 payload 中提取标题
fn extract_title_from_payload(payload: &serde_json::Value) -> String {
  payload
    .get("title")
    .and_then(|v| v.as_str())
    .unwrap_or("系统通知")
    .to_string()
}

/// 从通知 payload 中提取消息内容
fn extract_message_from_payload(payload: &serde_json::Value) -> String {
  payload
    .get("message")
    .and_then(|v| v.as_str())
    .unwrap_or("")
    .to_string()
}

#[allow(clippy::too_many_arguments)]
#[inline]
async fn start_connect(
  request: &HttpRequest,
  payload: Payload,
  state: &Data<AppState>,
  jwt_secret: &Data<Secret<String>>,
  server: Data<RealtimeServerAddr>,
  access_token: String,
  device_id: String,
  client_app_version: Version,
  connect_at: i64,
) -> Result<HttpResponse> {
  let auth = authorization_from_token(access_token.as_str(), jwt_secret)?;
  let user_uuid = UserUuid::from_auth(auth)?;
  let result = state.user_cache.get_user_uid(&user_uuid).await;

  match result {
    Ok(uid) => {
      debug!(
        "🚀new websocket connecting: uid={}, device_id={}, client_version:{}",
        uid, device_id, client_app_version
      );

      let session_id = uuid::Uuid::new_v4().to_string();
      let realtime_user = RealtimeUser::new(
        uid,
        device_id,
        session_id,
        connect_at,
        client_app_version.to_string(),
      );
      let (tx, external_source) = mpsc::channel(100);
      let client = RealtimeClient::new(
        realtime_user,
        server.get_ref().clone(),
        Duration::from_secs(state.config.websocket.heartbeat_interval as u64),
        Duration::from_secs(state.config.websocket.client_timeout as u64),
        client_app_version,
        external_source,
        10,
      );

      // Receive user change notifications and send them to the client.
      listen_on_user_change(state, uid, tx.clone());

      // Receive system notifications and send them to the client.
      // Also delivers any pending (unprocessed) notifications that arrived while the client was offline.
      listen_on_system_notification(state, uid, tx);

      match ws::WsResponseBuilder::new(client, request, payload)
        .frame_size(MAX_FRAME_SIZE * 2)
        .start()
      {
        Ok(response) => {
          trace!("🔵ws connection established: uid={}", uid);
          Ok(response)
        },
        Err(e) => {
          error!("🔴ws connection error: {:?}", e);
          Err(e)
        },
      }
    },
    Err(err) => {
      if err.is_record_not_found() {
        return Ok(HttpResponse::NotFound().json("user not found"));
      }
      Err(AppResponseError::from(err).into())
    },
  }
}

fn listen_on_user_change(state: &Data<AppState>, uid: i64, tx: Sender<RealtimeMessage>) {
  let mut user_change_recv = state.pg_listeners.subscribe_user_change(uid);
  actix::spawn(async move {
    while let Some(notification) = user_change_recv.recv().await {
      // Extract the user object from the notification payload.
      if let Some(user) = notification.payload {
        trace!("Receive user change: {:?}", user);
        // Since bincode serialization is used for RealtimeMessage but does not support the
        // Serde `deserialize_any` method, the user metadata is serialized into a JSON string.
        // This step ensures compatibility and flexibility for the metadata field.
        let metadata = serde_json::to_string(&user.metadata).ok();
        // Construct a UserMessage with the user's details, including the serialized metadata.
        let msg = UserMessage::ProfileChange(AFUserChange {
          uid: user.uid,
          name: user.name,
          email: user.email,
          metadata,
        });
        if tx.send(RealtimeMessage::User(msg)).await.is_err() {
          break;
        }
      }
    }
  });
}

/// 监听系统通知并发送给客户端（v1 版本 WebSocket）
/// 连接建立时先补发离线期间未处理的通知，再持续监听新通知
fn listen_on_system_notification(state: &Data<AppState>, uid: i64, tx: Sender<RealtimeMessage>) {
  let mut notification_recv = state.pg_listeners.subscribe_system_notification(uid);
  let pg_pool = state.pg_pool.clone();
  actix::spawn(async move {
    // 补发离线期间积压的未处理通知
    match get_pending_notifications(&pg_pool, uid).await {
      Ok(pending) if !pending.is_empty() => {
        debug!(
          "Delivering {} pending notifications to uid={} on reconnect",
          pending.len(),
          uid
        );
        let mut delivered_ids = Vec::with_capacity(pending.len());
        for notification in pending {
          let msg = UserMessage::SystemNotification(AFSystemNotification {
            id: notification.id.to_string(),
            workspace_id: notification
              .workspace_id
              .map(|id| id.to_string())
              .unwrap_or_default(),
            notification_type: notification.notification_type,
            title: extract_title_from_payload(&notification.payload),
            message: extract_message_from_payload(&notification.payload),
            payload_json: notification.payload.to_string(),
            created_at: notification.created_at.timestamp(),
            recipient_uid: notification.recipient_uid.unwrap_or(0),
          });
          if tx.send(RealtimeMessage::User(msg)).await.is_err() {
            return;
          }
          delivered_ids.push(notification.id);
        }
        // 标记已推送的通知为已处理
        if let Err(e) = mark_notifications_processed(&pg_pool, &delivered_ids).await {
          error!("Failed to mark pending notifications as processed for uid={}: {:?}", uid, e);
        }
      },
      Ok(_) => {},
      Err(e) => {
        error!("Failed to fetch pending notifications for uid={}: {:?}", uid, e);
      },
    }

    // 持续监听新到达的通知
    while let Some(notification) = notification_recv.recv().await {
      debug!(
        "[ws_v1] Pushing system notification to uid={}: type={}, id={}",
        uid,
        notification.notification_type,
        notification.id
      );
      // 标记新通知为已处理
      if let Err(e) = mark_notifications_processed(&pg_pool, &[notification.id]).await {
        error!("Failed to mark notification {} as processed: {:?}", notification.id, e);
      }
      // 构造 AFSystemNotification 并通过 UserMessage 发送
      let msg = UserMessage::SystemNotification(AFSystemNotification {
        id: notification.id.to_string(),
        workspace_id: notification
          .workspace_id
          .map(|id| id.to_string())
          .unwrap_or_default(),
        notification_type: notification.notification_type,
        title: extract_title_from_payload(&notification.payload),
        message: extract_message_from_payload(&notification.payload),
        payload_json: notification.payload.to_string(),
        created_at: notification.created_at.timestamp(),
        recipient_uid: notification.recipient_uid.unwrap_or(0),
      });
      if tx.send(RealtimeMessage::User(msg)).await.is_err() {
        break;
      }
    }
  });
}

struct ConnectInfo {
  access_token: String,
  client_version: Version,
  device_id: String,
  connect_at: i64,
}

const CLIENT_VERSION: &str = "client-version";
const DEVICE_ID: &str = "device-id";
const CONNECT_AT: &str = "connect-at";

// Trait for parameter extraction
trait ExtractParameter {
  fn extract_param(&self, key: &str) -> Result<String, AppError>;
}

// Implement the trait for HashMap<String, String>
impl ExtractParameter for HashMap<String, String> {
  fn extract_param(&self, key: &str) -> Result<String, AppError> {
    self
      .get(key)
      .ok_or_else(|| {
        AppError::InvalidRequest(format!("Parameter with given key:{} not found", key))
      })
      .map(|s| s.to_string())
  }
}

// Implement the trait for HttpRequest
impl ExtractParameter for HttpRequest {
  fn extract_param(&self, key: &str) -> Result<String, AppError> {
    self
      .headers()
      .get(key)
      .ok_or_else(|| AppError::InvalidRequest(format!("Header with given key:{} not found", key)))
      .and_then(|value| {
        value
          .to_str()
          .map_err(|_| {
            AppError::InvalidRequest(format!("Invalid header value for given key:{}", key))
          })
          .map(|s| s.to_string())
      })
  }
}

impl ConnectInfo {
  fn parse_from<T: ExtractParameter>(source: &T) -> Result<Self, AppError> {
    let access_token = source.extract_param(AUTHORIZATION.as_str())?;
    let client_version_str = source.extract_param(CLIENT_VERSION)?;
    let client_version = Version::parse(&client_version_str)
      .map_err(|_| AppError::InvalidRequest(format!("Invalid version:{}", client_version_str)))?;
    let device_id = source.extract_param(DEVICE_ID)?;
    let connect_at = match source.extract_param(CONNECT_AT) {
      Ok(start_at) => start_at
        .parse::<i64>()
        .unwrap_or_else(|_| chrono::Utc::now().timestamp()),
      Err(_) => chrono::Utc::now().timestamp(),
    };

    Ok(Self {
      access_token,
      client_version,
      device_id,
      connect_at,
    })
  }
}

struct WsConnectionV2Params {
  access_token: String,
  device_id: String,
  client_id: u64,
  last_message_id: Option<MessageId>,
}

impl WsConnectionV2Params {
  fn parse(req: &HttpRequest) -> Result<Self, AppError> {
    let url = req.full_url();
    let query = url.query_pairs().collect::<HashMap<_, _>>();

    let access_token = Self::from_url(&query, "token")
      .ok_or_else(|| AppError::InvalidRequest("Missing access token".into()))?;
    let device_id = Self::from_url(&query, "deviceId")
      .ok_or_else(|| AppError::InvalidRequest("Missing device id".into()))?;
    let client_id: u64 = Self::from_url(&query, "clientId")
      .and_then(|id| id.parse().ok())
      .ok_or_else(|| AppError::InvalidRequest("Missing client id".into()))?;
    let last_message_id = Self::from_url(&query, "lastMessageId");
    let last_message_id = match last_message_id {
      None => None,
      Some(message_id) => Some(MessageId::try_from(message_id).map_err(|_| {
        AppError::InvalidRequest("Couldn't parse 'X-AF-Last-Message-ID' head value".into())
      })?),
    };
    Ok(WsConnectionV2Params {
      access_token,
      device_id,
      client_id,
      last_message_id,
    })
  }

  fn from_url(url_params: &HashMap<Cow<str>, Cow<str>>, param: &str) -> Option<String> {
    // we use params provided from URL as a backup since browser API doesn't allow to
    // establish WebSocket connection with custom HTTP headers
    let value = url_params.get(param).cloned()?;
    Some(value.to_string())
  }
}
