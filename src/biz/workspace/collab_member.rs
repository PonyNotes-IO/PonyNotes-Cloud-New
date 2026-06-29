use access_control::collab::CollabAccessControl;
use app_error::AppError;
use database::workspace::{
  insert_collab_member, select_collab_owner, select_permission, update_collab_member_permission,
  update_collab_member_invite_permission,
};
use database_entity::dto::AFAccessLevel;
use sqlx::PgPool;
use std::ops::DerefMut;
use std::sync::Arc;
use uuid::Uuid;

use crate::biz::notification::ops::create_workspace_notification;
use database::collab::{delete_collab_member, delete_collab_member_invite};

fn permission_name(permission_id: i32) -> &'static str {
  match permission_id {
    2 => "评论",
    3 => "编辑",
    4 => "完全访问",
    _ => "查看",
  }
}

async fn select_shared_view_name(pg_pool: &PgPool, view_id: &Uuid, uid: i64) -> Option<String> {
  sqlx::query_scalar(
    r#"
      SELECT name
      FROM af_collab_member_invite
      WHERE oid = $1 AND received_uid = $2
      LIMIT 1
    "#,
  )
  .bind(view_id.to_string())
  .bind(uid)
  .fetch_optional(pg_pool)
  .await
  .unwrap_or(None)
}

async fn notify_collab_permission_changed(
  pg_pool: &PgPool,
  workspace_id: &Uuid,
  view_id: &Uuid,
  uid: i64,
  view_name: &str,
  old_permission_id: Option<i32>,
  new_permission_id: Option<i32>,
  access_removed: bool,
) {
  let title = "文档权限已变�?;
  let message = if access_removed {
    format!("您对文档「{}」的访问权限已被移除，请知悉", view_name)
  } else if let Some(new_permission_id) = new_permission_id {
    let new_perm_name = permission_name(new_permission_id);
    if let Some(old_permission_id) = old_permission_id {
      format!(
        "您对文档「{}」的权限已从「{}」调整为「{}」，请知�?,
        view_name,
        permission_name(old_permission_id),
        new_perm_name
      )
    } else {
      format!(
        "您对文档「{}」的权限已调整为「{}」，请知�?,
        view_name, new_perm_name
      )
    }
  } else {
    format!("您对文档「{}」的权限已变更，请知�?, view_name)
  };

  let payload = serde_json::json!({
    "view_id": view_id.to_string(),
    "view_name": view_name,
    "title": title,
    "message": message,
    "old_permission": old_permission_id.map(permission_name),
    "old_permission_id": old_permission_id,
    "new_permission": new_permission_id.map(permission_name),
    "new_permission_id": new_permission_id,
    "event": if access_removed { "access_removed" } else { "permission_updated" },
  });

  if let Err(err) = create_workspace_notification(
    pg_pool,
    workspace_id,
    "collab_permission_changed",
    &payload,
    Some(uid),
  )
  .await
  {
    tracing::warn!(
      "Failed to send permission change notification to uid={}: {:?}",
      uid,
      err
    );
  }
}

pub async fn add_collab_member(
  pg_pool: &PgPool,
  access_control: Arc<dyn CollabAccessControl>,
  workspace_id: &Uuid,
  view_id: &Uuid,
  send_uid: i64,
  received_uid: i64,
  view_name: &str,
  permission_id: i32,
) -> Result<(), AppError> {
  let mut tx = pg_pool.begin().await?;

  let owner_id = select_collab_owner(tx.deref_mut(), workspace_id, view_id).await?;

  if owner_id == received_uid {
    return Err(AppError::InvalidRequest(
      "被邀请者不能是笔记所有�?.to_string(),
    ));
  }

  // 使用传入�?permission_id 参数，同时传�?workspace_id 以记�?owner_workspace_id
  insert_collab_member(
    &mut tx,
    view_id,
    send_uid,
    received_uid,
    view_name,
    permission_id,
    workspace_id,
  )
  .await?;

  // �?permission_id 转换�?AFAccessLevel
  let access_level = match permission_id {
    2 => AFAccessLevel::ReadAndComment,
    3 => AFAccessLevel::ReadAndWrite,
    4 => AFAccessLevel::FullAccess,
    _ => AFAccessLevel::ReadOnly,
  };

  access_control
    .update_access_level_policy(&received_uid, &view_id, access_level)
    .await?;
  tx.commit().await?;

  // 权限名称
  let perm_name = permission_name(permission_id);

  // 通知被分享的用户（B�?
  let sender_name = database::user::select_name_from_uid(pg_pool, send_uid)
    .await
    .unwrap_or_else(|_| "用户".to_string());
  let payload_b = serde_json::json!({
    "view_id": view_id.to_string(),
    "view_name": view_name,
    "shared_by": send_uid,
    "permission": perm_name,
    "title": "收到一篇分享笔�?,
    "message": format!("【{}】将笔记「{}」分享给了你，赋予「{}」权限，请及时查�?, sender_name, view_name, perm_name),
  });
  if let Err(err) = create_workspace_notification(
    pg_pool,
    workspace_id,
    "collab_shared",
    &payload_b,
    Some(received_uid),
  )
  .await
  {
    tracing::warn!(
      "Failed to send collab share notification to uid={}: {:?}",
      received_uid,
      err
    );
  }

  // 通知分享者（A）：分享成功确认
  let receiver_name = database::user::select_name_from_uid(pg_pool, received_uid)
    .await
    .unwrap_or_else(|_| "对方用户".to_string());
  let payload_a = serde_json::json!({
    "view_id": view_id.to_string(),
    "view_name": view_name,
    "shared_to": received_uid,
    "permission": perm_name,
    "title": "笔记分享成功",
    "message": format!("你已将笔记「{}」分享给【{}】，已赋予「{}」权�?, view_name, receiver_name, perm_name),
  });
  if let Err(err) = create_workspace_notification(
    pg_pool,
    workspace_id,
    "collab_share_sent",
    &payload_a,
    Some(send_uid),
  )
  .await
  {
    tracing::warn!(
      "Failed to send collab share sent notification to uid={}: {:?}",
      send_uid,
      err
    );
  }

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
  
  // 检查是否有权修改权�?
  if owner_uid != owner_id {
    return Err(AppError::NotEnoughPermissions);
  }

  let permission = select_permission(pg_pool, new_permission_id)
    .await?
    .ok_or(AppError::InvalidRequest("无效的权限id".to_string()))?;

  // 查询旧权限用于通知
  let old_permission_id: Option<i32> = sqlx::query_scalar(
    "SELECT permission_id FROM af_collab_member WHERE oid = $1 AND uid = $2",
  )
  .bind(view_id.to_string())
  .bind(uid)
  .fetch_optional(pg_pool)
  .await
  .unwrap_or(None);

  update_collab_member_permission(pg_pool, view_id, uid, new_permission_id).await?;

  // 同步更新 af_collab_member_invite 表中的权限，保持邀请记录与实际权限一�?
  update_collab_member_invite_permission(pg_pool, view_id, uid, new_permission_id).await?;

  // 同步更新 Casbin 权限策略
  access_control
    .update_access_level_policy(&uid, &view_id, permission.access_level)
    .await?;

  // 发送权限变更通知给被修改权限的用�?
  let view_name = select_shared_view_name(pg_pool, view_id, uid).await;
  let doc_name = view_name.as_deref().unwrap_or("未知文章");
  notify_collab_permission_changed(
    pg_pool,
    workspace_id,
    view_id,
    uid,
    doc_name,
    old_permission_id,
    Some(new_permission_id),
    false,
  )
  .await;

  Ok(())
}

/// 删除协作成员
///
/// # 参数
/// * `pg_pool` - PostgreSQL 连接�?
/// * `access_control` - 协作访问控制
/// * `workspace_id` - 工作�?ID
/// * `view_id` - 文档 ID
/// * `owner_uid` - 操作�?UID（必须是文档拥有者）
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
  // 1. 检查操作者是否是文档拥有�?
  let owner_id = select_collab_owner(pg_pool, workspace_id, view_id).await?;

  if owner_uid != owner_id {
    return Err(AppError::NotEnoughPermissions);
  }

  // 2. 不能删除自己
  if owner_uid == uid {
    return Err(AppError::InvalidRequest("不能移除自己".to_string()));
  }

  let old_permission_id: Option<i32> = sqlx::query_scalar(
    "SELECT permission_id FROM af_collab_member WHERE oid = $1 AND uid = $2",
  )
  .bind(oid)
  .bind(uid)
  .fetch_optional(pg_pool)
  .await
  .unwrap_or(None);
  let view_name = select_shared_view_name(pg_pool, view_id, uid).await;

  // Remove the actual access grant. Keep af_collab_member_invite as the owner's
  // sent-share metadata; /me/received is filtered through af_collab_member.
  delete_collab_member(pg_pool, uid, oid).await?;
  let doc_name = view_name.as_deref().unwrap_or("未知文章");

  // 3. 删除 af_collab_member 表中的记�?  delete_collab_member(pg_pool, uid, oid).await?;

  // 4. 同步删除 af_collab_member_invite 中的邀请记�?
  let _ = sqlx::query(
    "UPDATE af_collab_member_invite SET permission_id = permission_id WHERE oid = $1 AND received_uid = $2 AND FALSE",
  )
  .bind(oid)
  .bind(uid)
  .execute(pg_pool)
  .await;

  // 5. 删除 Casbin 访问控制策略
  access_control
    .remove_access_level(&uid, view_id)
    .await?;

  // Marks a full access revocation (vs. a mere permission downgrade). The
  // client uses this to drop the view from the recipient's shared list and
  // close its open tab, while leaving other users' lists untouched.
  notify_collab_permission_changed(
    pg_pool,
    workspace_id,
    view_id,
    uid,
    doc_name,
    old_permission_id,
    None,
    true,
  )
  .await;

  Ok(())
}

/// 删除协作邀请（取消邀请）
///
/// # 参数
/// * `pg_pool` - PostgreSQL 连接�?
/// * `send_uid` - 发送�?UID（必须是邀请者）
/// * `received_uid` - 接收�?UID
/// * `oid` - 协作对象 ID
pub async fn remove_collab_member_invite(
  pg_pool: &PgPool,
  send_uid: i64,
  received_uid: i64,
  oid: &str,
) -> Result<(), AppError> {
  // 删除邀请记�?
  delete_collab_member_invite(pg_pool, send_uid, received_uid, oid).await?;

  Ok(())
}
