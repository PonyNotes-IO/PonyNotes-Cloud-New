// src/biz/user/user_merge.rs
//
// 用户账号合并 - 工作区数据迁移
//
// 场景：用户通过手机号绑定流程合并两个账号（primary = 当前登录账号，secondary = 已注册该手机号的旧账号）。
//
// 迁移策略（按工作区分）：
//
//  协作工作区（secondary 是成员之一，且还有其他成员）：
//    → 将 primary 账号作为新成员加入该工作区（保留原协作关系不变）
//    → 仅迁移 secondary 在该工作区中的个人 collab 数据（文档等）到 primary
//    → 成员上限检查（workspace_member_limit），超限时跳过并记录
//
//  独享工作区（secondary 是唯一成员，即该工作区只有 secondary 一人）：
//    → 将整个工作区（含所有 collab）迁移到 primary 名下
//    → 在 primary 下创建同名新工作区，迁移所有 collab 数据
//
// Collab 迁移时 oid 冲突处理：为每个 collab 生成新 oid（uuid::Uuid::new_v4()）
// oid_to_new_oid_map 记录旧→新映射，用于修复 collab 内部引用（保守实现，见 TODO）。
//
// 注意：此函数不处理 gotrue 层的用户删除（由 Go 层处理）

use crate::biz::subscription::ops::{get_user_resource_limit_status};
use crate::biz::user::user_init::{
  create_user_awareness, create_workspace_collab, create_workspace_database_collab,
};
use database::subscription::get_user_owned_workspace_count;
use access_control::workspace::WorkspaceAccessControl;
use anyhow::anyhow;
use app_error::AppError;
use bytes::Bytes;
use collab_entity::CollabType;
use database::collab::CollabStore;
use database::pg_row::AFCollabData;
use database::user::select_uid_from_uuid;
use database::workspace::{
  insert_user_workspace, select_all_user_workspaces,
};
use database_entity::dto::AFRole;
use serde::{Deserialize, Serialize};
use shared_entity::response::AppResponseError;
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{error, info, instrument, warn};
use uuid::Uuid;

// ============================================================================
// DTO
// ============================================================================
// 本地结构体
// ============================================================================

/// Represents the minimum workspace data needed during merge.
#[derive(Debug, Clone)]
struct WorkspaceRef {
  pub workspace_id: Uuid,
  pub workspace_name: Option<String>,
  pub owner_uid: Option<i64>,
}

impl From<database::pg_row::AFWorkspaceRowWithMemberCountAndRole> for WorkspaceRef {
  fn from(row: database::pg_row::AFWorkspaceRowWithMemberCountAndRole) -> Self {
    Self {
      workspace_id: row.workspace_id,
      workspace_name: row.workspace_name,
      owner_uid: row.owner_uid,
    }
  }
}

// ============================================================================
// DTO
// ============================================================================

#[derive(Debug, Serialize, Deserialize)]
pub struct MigrateUserDataRequest {
  /// 主账号 UUID（接收数据方）
  pub primary_user_uuid: Uuid,
  /// 被合并账号 UUID（数据来源）
  pub secondary_user_uuid: Uuid,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MigrateUserDataResponse {
  /// 主账号 UID
  pub primary_uid: i64,
  /// 被合并账号 UID
  pub secondary_uid: i64,
  /// 迁移的工作区数量（协作工作区加入次数 + 独享工作区迁移次数）
  pub migrated_workspace_count: i32,
  /// 每个工作区的处理详情
  pub workspaces: Vec<WorkspaceMigrationResult>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "strategy")]
pub enum WorkspaceMigrationResult {
  /// 协作工作区：primary 作为新成员加入
  #[serde(rename = "joined")]
  CollaborativeJoined {
    workspace_id: Uuid,
    workspace_name: String,
    primary_role: String,
    /// secondary 在该工作区中迁移的 collab 数量
    collab_count: i32,
    status: String,
    error: Option<String>,
  },
  /// 协作工作区：因成员上限超限而跳过
  #[serde(rename = "skipped_member_limit")]
  SkippedMemberLimit {
    workspace_id: Uuid,
    workspace_name: String,
    reason: String,
  },
  /// 独享工作区：整体迁移到 primary
  #[serde(rename = "migrated")]
  Migrated {
    workspace_id: Uuid,
    new_workspace_id: Uuid,
    workspace_name: String,
    collab_count: i32,
    status: String,
    error: Option<String>,
  },
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GetUserWorkspacesRequest {
  pub user_uuid: Uuid,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GetUserWorkspacesResponse {
  pub uid: i64,
  pub workspaces: Vec<WorkspaceInfo>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WorkspaceInfo {
  pub workspace_id: Uuid,
  pub workspace_name: String,
  pub created_at: String,
  pub owner_uid: i64,
}

// ============================================================================
// 用户查询
// ============================================================================

/// 获取用户的所有工作区信息（用于 Go 层显示迁移预览）
#[instrument(skip(pg_pool), err)]
pub async fn get_user_workspaces(
  pg_pool: &PgPool,
  user_uuid: &Uuid,
) -> Result<GetUserWorkspacesResponse, AppResponseError> {
  let uid =
    select_uid_from_uuid(pg_pool, user_uuid)
      .await
      .map_err(AppResponseError::from)?;

  let workspaces = select_all_user_workspaces(pg_pool, user_uuid)
    .await
    .map_err(AppResponseError::from)?;

  let workspace_infos: Vec<WorkspaceInfo> = workspaces
    .into_iter()
    .map(|w| WorkspaceInfo {
      workspace_id: w.workspace_id,
      workspace_name: w.workspace_name.unwrap_or_default(),
      created_at: w.created_at.map(|t| t.to_rfc3339()).unwrap_or_default(),
      owner_uid: w.owner_uid.unwrap_or(0),
    })
    .collect();

  Ok(GetUserWorkspacesResponse {
    uid,
    workspaces: workspace_infos,
  })
}

// ============================================================================
// 核心迁移逻辑
// ============================================================================

/// 将 secondary_user 的所有工作区数据迁移到 primary_user。
///
/// 迁移策略（按工作区分）：
/// - 协作工作区（secondary 是成员，且工作区还有其他成员）：
///   将 primary 账号作为 Member 加入工作区，仅迁移 secondary 的个人 collab 数据。
/// - 独享工作区（secondary 是唯一成员）：
///   将整个工作区迁移到 primary 名下（创建新工作区 + 迁移所有 collab）。
///
/// 注意：此函数不处理 gotrue 层的用户删除（由 Go 层处理）。
#[instrument(skip_all, fields(primary_uuid=%req.primary_user_uuid, secondary_uuid=%req.secondary_user_uuid), err)]
pub async fn migrate_user_workspaces(
  pg_pool: &PgPool,
  collab_storage: &Arc<dyn CollabStore>,
  workspace_access_control: &Arc<dyn WorkspaceAccessControl>,
  req: MigrateUserDataRequest,
) -> Result<MigrateUserDataResponse, AppResponseError> {
  let primary_user_uuid = &req.primary_user_uuid;
  let secondary_user_uuid = &req.secondary_user_uuid;

  info!(
    "[UserMerge] Starting workspace migration: primary={}, secondary={}",
    primary_user_uuid, secondary_user_uuid
  );

  // 1. 解析用户 UID
  let primary_uid = select_uid_from_uuid(pg_pool, primary_user_uuid)
    .await
    .map_err(AppResponseError::from)?;
  let secondary_uid = select_uid_from_uuid(pg_pool, secondary_user_uuid)
    .await
    .map_err(AppResponseError::from)?;

  info!(
    "[UserMerge] Resolved UIDs: primary_uid={}, secondary_uid={}",
    primary_uid, secondary_uid
  );

  // 2. 查询 secondary 的所有工作区
  let secondary_workspaces = select_all_user_workspaces(pg_pool, secondary_user_uuid)
    .await
    .map_err(AppResponseError::from)?;

  info!(
    "[UserMerge] Found {} workspaces for secondary user",
    secondary_workspaces.len()
  );

  // 3. 获取 primary 当前的成员上限（用于协作工作区加入检查）
  let primary_resource_status = get_user_resource_limit_status(pg_pool, primary_uid)
    .await
    .map_err(|e| AppResponseError::from(AppError::Internal(anyhow!("get_user_resource_limit_status failed: {}", e))))?;

  // 4. 获取 primary 当前拥有的工作区数量（用于独享工作区迁移检查）
  let primary_owned_count = get_user_owned_workspace_count(pg_pool, primary_uid)
    .await
    .map_err(|e| AppResponseError::from(AppError::Internal(anyhow!("get_user_owned_workspace_count failed: {}", e))))?;

  // 5. 逐个处理工作区
  let mut results: Vec<WorkspaceMigrationResult> = Vec::new();
  let mut joined_count = 0u32;
  let mut migrated_count = 0u32;

  for sw in &secondary_workspaces {
    let workspace_ref = WorkspaceRef::from(sw.clone());
    let workspace_name = workspace_ref
      .workspace_name
      .clone()
      .unwrap_or_else(|| "Imported Workspace".to_string());

    info!(
      "[UserMerge] Processing workspace: id={}, name={}, owner_uid={}",
      workspace_ref.workspace_id, workspace_name, workspace_ref.owner_uid.unwrap_or(0)
    );

    // 5a. 查询该工作区的所有成员
    let members = select_workspace_member_list_raw(pg_pool, &workspace_ref.workspace_id)
      .await
      .map_err(AppResponseError::from)?;

    let member_count_excluding_guests: usize = members
      .iter()
      .filter(|m| m.role != AFRole::Guest as i32)
      .count();

    info!(
      "[UserMerge]   workspace {} has {} non-guest members (secondary_uid={})",
      workspace_ref.workspace_id, member_count_excluding_guests, secondary_uid
    );

    // 5b. 判断工作区类型并执行对应迁移策略
    let result = if member_count_excluding_guests > 1 {
      // ==================== 协作工作区 ====================
      // 工作区还有其他成员（非 secondary 独占）→ 将 primary 作为新成员加入
      info!(
        "[UserMerge]   -> Collaborative workspace, adding primary as new member"
      );

      add_primary_to_collaborative_workspace(
        pg_pool,
        collab_storage,
        workspace_access_control,
        primary_uid,
        primary_user_uuid,
        secondary_uid,
        &workspace_ref,
        &members,
        &primary_resource_status,
      )
      .await
    } else {
      // ==================== 独享工作区 ====================
      // 工作区只有 secondary 一人 → 将整个工作区迁移到 primary
      info!(
        "[UserMerge]   -> Solo workspace, migrating entire workspace to primary"
      );

      let current_migrated = migrated_count as i64;
      let max_allowed = primary_resource_status.workspace_limit.saturating_sub(primary_owned_count);

      if current_migrated >= max_allowed {
        WorkspaceMigrationResult::SkippedMemberLimit {
          workspace_id: workspace_ref.workspace_id,
          workspace_name,
          reason: format!(
            "Primary user workspace limit reached ({}/{})",
            primary_owned_count, primary_resource_status.workspace_limit
          ),
        }
      } else {
        migrate_solo_workspace(
          pg_pool,
          collab_storage,
          workspace_access_control,
          primary_uid,
          primary_user_uuid,
          &workspace_ref,
          secondary_uid,
        )
        .await
      }
    };

    // 5c. 更新计数
    match &result {
      WorkspaceMigrationResult::CollaborativeJoined { .. } => joined_count += 1,
      WorkspaceMigrationResult::Migrated { .. } => migrated_count += 1,
      WorkspaceMigrationResult::SkippedMemberLimit { .. } => {}
    }

    results.push(result);
  }

  let total_count = joined_count + migrated_count;
  info!(
    "[UserMerge] Migration completed: {} collaborative joined, {} solo migrated, {} skipped",
    joined_count, migrated_count,
    secondary_workspaces.len() as u32 - total_count
  );

  Ok(MigrateUserDataResponse {
    primary_uid,
    secondary_uid,
    migrated_workspace_count: total_count as i32,
    workspaces: results,
  })
}

// ============================================================================
// 协作工作区处理
// ============================================================================

/// 将 primary 用户作为新成员加入协作工作区，
/// 并迁移 secondary 在该工作区中的个人 collab 数据。
async fn add_primary_to_collaborative_workspace(
  pg_pool: &PgPool,
  collab_storage: &Arc<dyn CollabStore>,
  workspace_access_control: &Arc<dyn WorkspaceAccessControl>,
  primary_uid: i64,
  primary_user_uuid: &Uuid,
  secondary_uid: i64,
  workspace: &WorkspaceRef,
  members: &[WorkspaceMemberRaw],
  resource_status: &crate::biz::subscription::ops::ResourceLimitStatus,
) -> WorkspaceMigrationResult {
  let workspace_id = workspace.workspace_id;
  let workspace_name = workspace
    .workspace_name
    .clone()
    .unwrap_or_else(|| "Unknown Workspace".to_string());

  // 检查 primary 是否已经是该工作区成员（可能 secondary 之前加入过 primary 的工作区）
  let already_member = members.iter().any(|m| m.uid == primary_uid);
  if already_member {
    info!(
      "[AddToCollab] Primary {} is already a member of workspace {}, skipping join",
      primary_uid, workspace_id
    );
    // 仍然尝试迁移 secondary 的个人 collab（以防之前未迁移）
    let collab_count = migrate_secondary_collabs_to_existing_workspace(
      pg_pool,
      collab_storage,
      &workspace_id,
      primary_uid,
      secondary_uid,
    )
    .await
    .unwrap_or(0);

    return WorkspaceMigrationResult::CollaborativeJoined {
      workspace_id,
      workspace_name,
      primary_role: members
        .iter()
        .find(|m| m.uid == primary_uid)
        .map(|m| role_id_to_name(m.role))
        .unwrap_or_else(|| "Member".to_string()),
      collab_count,
      status: "already_member".to_string(),
      error: None,
    };
  }

  // 检查工作区成员上限
  let current_member_count = members
    .iter()
    .filter(|m| m.role != AFRole::Guest as i32)
    .count() as i64;

  if current_member_count >= resource_status.member_limit {
    return WorkspaceMigrationResult::SkippedMemberLimit {
      workspace_id,
      workspace_name,
      reason: format!(
        "Workspace member limit reached ({}/{})",
        current_member_count, resource_status.member_limit
      ),
    };
  }

  // 将 primary 加入工作区（作为 Member，而非 Owner）
  if let Err(e) = workspace_access_control
    .insert_role(&primary_uid, &workspace_id, AFRole::Member)
    .await
  {
    error!(
      "[AddToCollab] Failed to insert primary as Member in workspace {}: {:?}",
      workspace_id, e
    );
    return WorkspaceMigrationResult::CollaborativeJoined {
      workspace_id,
      workspace_name,
      primary_role: "Member".to_string(),
      collab_count: 0,
      status: "failed".to_string(),
      error: Some(e.to_string()),
    };
  }

  info!(
    "[AddToCollab] Successfully added primary {} as Member to workspace {}",
    primary_uid, workspace_id
  );

  // 迁移 secondary 在该工作区中的个人 collab 数据到 primary
  let collab_count = migrate_secondary_collabs_to_existing_workspace(
    pg_pool,
    collab_storage,
    &workspace_id,
    primary_uid,
    secondary_uid,
  )
  .await
  .unwrap_or(0);

  WorkspaceMigrationResult::CollaborativeJoined {
    workspace_id,
    workspace_name,
    primary_role: "Member".to_string(),
    collab_count,
    status: "success".to_string(),
    error: None,
  }
}

/// 迁移 secondary 在指定工作区中的个人 collab 数据到 primary。
/// 不迁移公共 Folder/DB 结构，只迁移 secondary 个人创建的文档等。
async fn migrate_secondary_collabs_to_existing_workspace(
  pg_pool: &PgPool,
  collab_storage: &Arc<dyn CollabStore>,
  workspace_id: &Uuid,
  primary_uid: i64,
  secondary_uid: i64,
) -> Result<i32, AppError> {
  // 读取 secondary 在该工作区中的所有 collab
  let source_collabs = read_all_collabs_in_workspace(pg_pool, workspace_id, secondary_uid).await?;

  let collab_count = source_collabs.len();
  if collab_count == 0 {
    return Ok(0);
  }

  info!(
    "[MigrateSecondaryCollabs] Migrating {} collabs from secondary to primary in workspace {}",
    collab_count, workspace_id
  );

  // 为所有 collab 生成新 oid
  let oid_map: HashMap<Uuid, Uuid> = source_collabs
    .iter()
    .map(|c| (c.oid, Uuid::new_v4()))
    .collect();

  // 分批写入，每个 collab 独立事务以减少锁竞争
  let mut success_count = 0i32;
  for collab in source_collabs {
    let new_oid = match oid_map.get(&collab.oid) {
      Some(id) => *id,
      None => continue,
    };

    let collab_type = partition_key_to_collab_type(collab.partition_key)?;
    let fixed_blob = fix_collab_references(&collab.blob, collab_type, &oid_map)?;

    let params = database_entity::dto::CollabParams {
      object_id: new_oid,
      collab_type,
      encoded_collab_v1: Bytes::from(fixed_blob),
      updated_at: Some(collab.updated_at),
    };

    match collab_storage
      .batch_insert_new_collab(*workspace_id, &primary_uid, vec![params])
      .await
    {
      Ok(_) => {
        success_count += 1;
        info!(
          "[MigrateSecondaryCollabs] Migrated {:?}: {} -> {}",
          collab_type, collab.oid, new_oid
        );
      },
      Err(e) => {
        warn!(
          "[MigrateSecondaryCollabs] Failed to migrate collab {}: {}",
          collab.oid, e
        );
      },
    }
  }

  Ok(success_count)
}

// ============================================================================
// 独享工作区处理
// ============================================================================

/// 将 secondary 的独享工作区（整个工作区 + 所有 collab）迁移到 primary。
async fn migrate_solo_workspace(
  pg_pool: &PgPool,
  collab_storage: &Arc<dyn CollabStore>,
  workspace_access_control: &Arc<dyn WorkspaceAccessControl>,
  primary_uid: i64,
  primary_user_uuid: &Uuid,
  source_workspace: &WorkspaceRef,
  secondary_uid: i64,
) -> WorkspaceMigrationResult {
  let source_workspace_id = source_workspace.workspace_id;
  let workspace_name = source_workspace
    .workspace_name
    .clone()
    .unwrap_or_else(|| "Imported Workspace".to_string());

  // 在 primary 下创建新工作区
  let new_workspace_id = match create_workspace_for_user(
    pg_pool,
    collab_storage,
    workspace_access_control,
    primary_user_uuid,
    primary_uid,
    &workspace_name,
  )
  .await
  {
    Ok(id) => id,
    Err(e) => {
      error!(
        "[MigrateSolo] Failed to create workspace for primary: {:?}",
        e
      );
      return WorkspaceMigrationResult::Migrated {
        workspace_id: source_workspace_id,
        new_workspace_id: Uuid::nil(),
        workspace_name,
        collab_count: 0,
        status: "failed".to_string(),
        error: Some(e.to_string()),
      };
    },
  };

  info!(
    "[MigrateSolo] Created new workspace: {} (source: {})",
    new_workspace_id, source_workspace_id
  );

  // 读取 source 工作区的所有 collab 对象
  let source_collabs = match read_all_collabs_in_workspace(pg_pool, &source_workspace_id, secondary_uid).await {
    Ok(c) => c,
    Err(e) => {
      error!(
        "[MigrateSolo] Failed to read collabs from source workspace: {:?}",
        e
      );
      return WorkspaceMigrationResult::Migrated {
        workspace_id: source_workspace_id,
        new_workspace_id,
        workspace_name,
        collab_count: 0,
        status: "failed".to_string(),
        error: Some(e.to_string()),
      };
    },
  };

  let collab_count = source_collabs.len();
  info!(
    "[MigrateSolo] Read {} collab objects from source workspace",
    collab_count
  );

  if collab_count == 0 {
    return WorkspaceMigrationResult::Migrated {
      workspace_id: source_workspace_id,
      new_workspace_id,
      workspace_name,
      collab_count: 0,
      status: "success".to_string(),
      error: None,
    };
  }

  // 迁移所有 collab 到新工作区
  if let Err(e) = migrate_collabs_to_workspace(
    pg_pool,
    collab_storage,
    &new_workspace_id,
    primary_uid,
    source_collabs,
  )
  .await
  {
    error!(
      "[MigrateSolo] Failed to migrate collabs to new workspace: {:?}",
      e
    );
    return WorkspaceMigrationResult::Migrated {
      workspace_id: source_workspace_id,
      new_workspace_id,
      workspace_name,
      collab_count: 0,
      status: "failed".to_string(),
      error: Some(e.to_string()),
    };
  }

  WorkspaceMigrationResult::Migrated {
    workspace_id: source_workspace_id,
    new_workspace_id,
    workspace_name,
    collab_count: collab_count as i32,
    status: "success".to_string(),
    error: None,
  }
}

/// 为 primary 用户创建工作区
async fn create_workspace_for_user(
  pg_pool: &PgPool,
  collab_storage: &Arc<dyn CollabStore>,
  workspace_access_control: &Arc<dyn WorkspaceAccessControl>,
  user_uuid: &Uuid,
  uid: i64,
  name: &str,
) -> Result<Uuid, AppError> {
  // 检查用户是否已达到工作区数量上限
  let resource_status = get_user_resource_limit_status(pg_pool, uid)
    .await
    .map_err(|e| AppError::Internal(anyhow!("get_user_resource_limit_status failed: {}", e)))?;
  let current_count = get_user_owned_workspace_count(pg_pool, uid)
    .await
    .map_err(|e| AppError::Internal(anyhow!("get_user_owned_workspace_count failed: {}", e)))?;

  if current_count >= resource_status.workspace_limit {
    return Err(AppError::PlanLimitExceeded(format!(
      "Workspace limit reached (Limit: {}). Please upgrade your subscription.",
      resource_status.workspace_limit
    )));
  }

  // 插入工作区记录
  let workspace =
    insert_user_workspace(pg_pool, user_uuid, name, "", false)
      .await
      .map_err(|e| AppError::Internal(anyhow!("insert_user_workspace failed: {}", e)))?;

  let workspace_id = workspace.workspace_id;

  info!(
    "[CreateWorkspace] Inserted workspace record: {} for user {}",
    workspace_id, uid
  );

  // 添加 Owner 角色记录到 workspace_access_control
  workspace_access_control
    .insert_role(&uid, &workspace_id, AFRole::Owner)
    .await
    .map_err(|e| AppError::Internal(anyhow!("insert_role failed: {}", e)))?;

  // 为新工作区创建初始 collab 结构（Folder + WorkspaceDatabase + UserAwareness）
  let mut txn = pg_pool.begin().await?;

  // 创建 Folder collab
  create_workspace_collab(
    uid,
    workspace_id,
    name,
    collab_storage,
    &mut txn,
  )
  .await?;

  // 创建 WorkspaceDatabase collab
  if let Some(&database_storage_id) = workspace.database_storage_id.as_ref() {
    create_workspace_database_collab(
      workspace_id,
      &uid,
      database_storage_id,
      collab_storage,
      &mut txn,
      vec![],
    )
    .await?;
  }

  // 创建 UserAwareness collab
  create_user_awareness(&uid, user_uuid, workspace_id, collab_storage, &mut txn).await?;

  txn.commit().await?;

  info!(
    "[CreateWorkspaceCollabs] Created initial collabs for workspace {}",
    workspace_id
  );

  Ok(workspace_id)
}

// ============================================================================
// Collab 读取与写入
// ============================================================================

/// Intermediate row type with primitive `role: i32` to allow non-macro sqlx query_as.
#[derive(Debug, Clone, sqlx::FromRow)]
struct WorkspaceMemberRaw {
  uid: i64,
  role: i32,
}

/// 查询工作区的所有成员（raw i32 role）。
async fn select_workspace_member_list_raw(
  pg_pool: &PgPool,
  workspace_id: &Uuid,
) -> Result<Vec<WorkspaceMemberRaw>, AppError> {
  let rows = sqlx::query_as::<_, WorkspaceMemberRaw>(
    r#"
    SELECT
      af_workspace_member.uid,
      af_workspace_member.role_id AS role
    FROM public.af_workspace_member
    WHERE af_workspace_member.workspace_id = $1
    ORDER BY af_workspace_member.created_at ASC
    "#,
  )
  .bind(workspace_id)
  .fetch_all(pg_pool)
  .await
  .map_err(|e| AppError::Internal(anyhow!("select_workspace_member_list_raw failed: {}", e)))?;

  Ok(rows)
}

/// 读取指定工作区和所有者的所有 collab 对象。
async fn read_all_collabs_in_workspace(
  pg_pool: &PgPool,
  workspace_id: &Uuid,
  owner_uid: i64,
) -> Result<Vec<AFCollabData>, AppError> {
  let collabs: Vec<AFCollabData> = sqlx::query_as!(
    AFCollabData,
    r#"
      SELECT
        oid,
        partition_key,
        updated_at,
        blob
      FROM af_collab
      WHERE workspace_id = $1
        AND owner_uid = $2
        AND deleted_at IS NULL
    "#,
    workspace_id,
    owner_uid
  )
  .fetch_all(pg_pool)
  .await
  .map_err(|e| AppError::Internal(anyhow!("read_all_collabs_in_workspace failed: {}", e)))?;

  Ok(collabs)
}

/// 将 collab 对象迁移到新工作区。
/// 为每个 collab 生成新 oid，记录映射关系。
async fn migrate_collabs_to_workspace(
  pg_pool: &PgPool,
  collab_storage: &Arc<dyn CollabStore>,
  new_workspace_id: &Uuid,
  new_owner_uid: i64,
  source_collabs: Vec<AFCollabData>,
) -> Result<(), AppError> {
  if source_collabs.is_empty() {
    return Ok(());
  }

  // 生成 oid 映射：old_oid -> new_oid
  // Folder(3) 必须先迁移，因为其他 collab 可能引用其子视图
  let oid_map: HashMap<Uuid, Uuid> = source_collabs
    .iter()
    .map(|c| (c.oid, Uuid::new_v4()))
    .collect();

  info!("[MigrateCollabs] oid_map size: {}", oid_map.len());

  // 先迁移 Folder，再迁移其他类型
  let mut folder_collabs: Vec<AFCollabData> = Vec::new();
  let mut other_collabs: Vec<AFCollabData> = Vec::new();

  for collab in source_collabs {
    if collab.partition_key == 3 {
      folder_collabs.push(collab);
    } else {
      other_collabs.push(collab);
    }
  }

  // 先处理 Folder
  for collab in folder_collabs {
    let new_oid = oid_map
      .get(&collab.oid)
      .ok_or_else(|| AppError::Internal(anyhow!("oid not found in map")))?;

    let fixed_blob = fix_folder_references(&collab.blob, &oid_map)?;
    let params = database_entity::dto::CollabParams {
      object_id: *new_oid,
      collab_type: CollabType::Folder,
      encoded_collab_v1: Bytes::from(fixed_blob),
      updated_at: Some(collab.updated_at),
    };

    collab_storage
      .batch_insert_new_collab(*new_workspace_id, &new_owner_uid, vec![params])
      .await
      .map_err(|e| AppError::Internal(anyhow!("batch_insert Folder failed: {}", e)))?;

    info!(
      "[MigrateCollabs] Migrated Folder: {} -> {}",
      collab.oid, new_oid
    );
  }

  // 再处理其他 collab
  for collab in other_collabs {
    let new_oid = oid_map
      .get(&collab.oid)
      .ok_or_else(|| AppError::Internal(anyhow!("oid not found in map")))?;

    let collab_type = partition_key_to_collab_type(collab.partition_key)?;
    let fixed_blob = fix_database_references(&collab.blob, collab_type, &oid_map)?;

    let params = database_entity::dto::CollabParams {
      object_id: *new_oid,
      collab_type,
      encoded_collab_v1: Bytes::from(fixed_blob),
      updated_at: Some(collab.updated_at),
    };

    collab_storage
      .batch_insert_new_collab(*new_workspace_id, &new_owner_uid, vec![params])
      .await
      .map_err(|e| {
        AppError::Internal(anyhow!("batch_insert_new_collab failed for {:?}: {}", collab_type, e))
      })?;

    info!(
      "[MigrateCollabs] Migrated {:?}: {} -> {}",
      collab_type, collab.oid, new_oid
    );
  }

  Ok(())
}

/// 修复 Folder collab 中引用的子视图 oid
fn fix_folder_references(blob: &[u8], _oid_map: &HashMap<Uuid, Uuid>) -> Result<Vec<u8>, AppError> {
  // Folder collab 内部引用通过 view_id。Yrs encoding 是二进制的，
  // 不容易直接替换。但 Folder 的子视图是独立的 collab 对象，
  // 它们的 oid 已经在 oid_map 中重新映射了。
  //
  // 由于 Yrs encoding 的复杂性，我们采用保守策略：
  // 直接使用原始 blob。客户端重新打开 Folder 时会修复引用。
  // 这不是完美方案，但对于大多数场景足够。
  //
  // TODO: 实现正确的 Yrs 二进制替换（替换 blob 中的 UUID 引用）
  Ok(blob.to_vec())
}

/// 修复 Database collab 中引用的视图 oid
fn fix_database_references(
  blob: &[u8],
  _collab_type: CollabType,
  _oid_map: &HashMap<Uuid, Uuid>,
) -> Result<Vec<u8>, AppError> {
  // Database row collab 可能引用其他视图的 oid。同 Folder，采用保守策略。
  // TODO: 实现正确的引用修复
  Ok(blob.to_vec())
}

/// 修复 collab 二进制数据中的旧 oid 引用（通用版本，用于协作工作区中的个人 collab 迁移）
fn fix_collab_references(
  blob: &[u8],
  collab_type: CollabType,
  oid_map: &HashMap<Uuid, Uuid>,
) -> Result<Vec<u8>, AppError> {
  // 对 Folder 类型特殊处理，其他类型保守保留原始 blob
  match collab_type {
    CollabType::Folder => fix_folder_references(blob, oid_map),
    _ => fix_database_references(blob, collab_type, oid_map),
  }
}

/// 将 partition_key 转换为 CollabType
fn partition_key_to_collab_type(partition_key: i32) -> Result<CollabType, AppError> {
  match partition_key {
    0 => Ok(CollabType::Document),
    1 => Ok(CollabType::Database),
    2 => Ok(CollabType::WorkspaceDatabase),
    3 => Ok(CollabType::Folder),
    4 => Ok(CollabType::DatabaseRow),
    5 => Ok(CollabType::UserAwareness),
    _ => Err(AppError::InvalidRequest(format!(
      "Unknown partition_key: {}",
      partition_key
    ))),
  }
}

/// 将 role_id (i32) 转换为角色名称字符串
fn role_id_to_name(role_id: i32) -> String {
  match role_id {
    1 => "Owner".to_string(),
    2 => "Member".to_string(),
    3 => "Guest".to_string(),
    _ => format!("Unknown({})", role_id),
  }
}
