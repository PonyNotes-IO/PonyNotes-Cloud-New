use access_control::collab::CollabAccessControl;
use app_error::AppError;
use database::workspace::{
  insert_collab_member, select_collab_owner, select_permission, update_collab_member_permission,
};
use database_entity::dto::AFAccessLevel;
use sqlx::PgPool;
use std::ops::DerefMut;
use std::sync::Arc;
use uuid::Uuid;

use database::collab::{delete_collab_member, delete_collab_member_invite};

pub async fn add_collab_member(
  pg_pool: &PgPool,
  access_control: Arc<dyn CollabAccessControl>,
  workspace_id: &Uuid,
  view_id: &Uuid,
  send_uid: i64,
  received_uid: i64,
  view_name: &str,
) -> Result<(), AppError> {
  let mut tx = pg_pool.begin().await?;

  let owner_id = select_collab_owner(tx.deref_mut(), workspace_id, view_id).await?;

  if owner_id == received_uid {
    return Err(AppError::InvalidRequest(
      "被邀请者不能是笔记所有者".to_string(),
    ));
  }

  insert_collab_member(&mut tx, view_id, send_uid, received_uid, view_name).await?;

  access_control
    .update_access_level_policy(&received_uid, &view_id, AFAccessLevel::ReadOnly)
    .await?;
  tx.commit().await?;
  Ok(())
}

pub async fn edit_collab_member_permission(
  pg_pool: &PgPool,
  access_control: Arc<dyn CollabAccessControl>,
  workspace_id: &Uuid,
  view_id: &Uuid,
  owner_uid: i64,
  uid: i64,
  new_permission_id: i32,
) -> Result<(), AppError> {
  let owner_id = select_collab_owner(pg_pool, workspace_id, view_id).await?;

  if owner_id == uid {
    return Err(AppError::InvalidRequest(
      "不能修改笔记所有者的权限".to_string(),
    ));
  }
  
  // 检查是否有权修改权限
  if owner_uid != owner_id {
    return Err(AppError::NotEnoughPermissions);
  }

  let permission = select_permission(pg_pool, new_permission_id)
    .await?
    .ok_or(AppError::InvalidRequest("无效的权限id".to_string()))?;

  update_collab_member_permission(pg_pool, view_id, uid, new_permission_id).await?;

  // 同步更新 Casbin 权限策略
  access_control
    .update_access_level_policy(&uid, &view_id, permission.access_level)
    .await?;

  Ok(())
}

/// 删除协作成员
///
/// # 参数
/// * `pg_pool` - PostgreSQL 连接池
/// * `access_control` - 协作访问控制
/// * `workspace_id` - 工作区 ID
/// * `view_id` - 文档 ID
/// * `owner_uid` - 操作者 UID（必须是文档拥有者）
/// * `uid` - 要删除的成员 UID
/// * `oid` - 协作对象 ID
pub async fn remove_collab_member(
  pg_pool: &PgPool,
  access_control: Arc<dyn CollabAccessControl>,
  workspace_id: &Uuid,
  view_id: &Uuid,
  owner_uid: i64,
  uid: i64,
  oid: &str,
) -> Result<(), AppError> {
  // 1. 检查操作者是否是文档拥有者
  let owner_id = select_collab_owner(pg_pool, workspace_id, view_id).await?;

  if owner_uid != owner_id {
    return Err(AppError::NotEnoughPermissions);
  }

  // 2. 不能删除自己
  if owner_uid == uid {
    return Err(AppError::InvalidRequest("不能移除自己".to_string()));
  }

  // 3. 删除 af_collab_member 表中的记录
  delete_collab_member(pg_pool, uid, oid).await?;

  // 4. 删除 Casbin 访问控制策略
  access_control
    .remove_access_level(&uid, view_id)
    .await?;

  Ok(())
}

/// 删除协作邀请（取消邀请）
///
/// # 参数
/// * `pg_pool` - PostgreSQL 连接池
/// * `send_uid` - 发送者 UID（必须是邀请者）
/// * `received_uid` - 接收者 UID
/// * `oid` - 协作对象 ID
pub async fn remove_collab_member_invite(
  pg_pool: &PgPool,
  send_uid: i64,
  received_uid: i64,
  oid: &str,
) -> Result<(), AppError> {
  // 删除邀请记录
  delete_collab_member_invite(pg_pool, send_uid, received_uid, oid).await?;

  Ok(())
}
