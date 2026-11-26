use app_error::AppError;
use database::collab::delete_collab_member_invite;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::biz::notification::ops::create_workspace_notification;

fn permission_name(permission_id: i32) -> &'static str {
  match permission_id {
    2 => "评论",
    3 => "编辑",
    4 => "完全访问",
    _ => "查看",
  }
}

pub async fn notify_collab_permission_changed(
  conn: &mut PgConnection,
  workspace_id: &Uuid,
  view_id: &Uuid,
  uid: i64,
  view_name: &str,
  old_permission_id: Option<i32>,
  new_permission_id: Option<i32>,
  access_removed: bool,
) -> Result<(), AppError> {
  let title = "文档权限已变更";
  let message = if access_removed {
    format!("您对文档「{}」的访问权限已被移除，请知悉", view_name)
  } else if let Some(new_permission_id) = new_permission_id {
    let new_perm_name = permission_name(new_permission_id);
    if let Some(old_permission_id) = old_permission_id {
      format!(
        "您对文档「{}」的权限已从「{}」调整为「{}」，请知悉",
        view_name,
        permission_name(old_permission_id),
        new_perm_name
      )
    } else {
      format!(
        "您对文档「{}」的权限已调整为「{}」，请知悉",
        view_name, new_perm_name
      )
    }
  } else {
    format!("您对文档「{}」的权限已变更，请知悉", view_name)
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

  create_workspace_notification(
    conn,
    workspace_id,
    "collab_permission_changed",
    &payload,
    Some(uid),
  )
  .await
}

pub async fn notify_member_added(
  pg_pool: &PgPool,
  workspace_id: &Uuid,
  view_id: &Uuid,
  send_uid: i64,
  received_uid: i64,
  view_name: &str,
  permission_id: i32,
) {
  // 权限名称
  let perm_name = permission_name(permission_id);

  // 通知被分享的用户（B）
  let sender_name = database::user::select_name_from_uid(pg_pool, send_uid)
    .await
    .unwrap_or_else(|_| "用户".to_string());
  let payload_b = serde_json::json!({
    "view_id": view_id.to_string(),
    "view_name": view_name,
    "shared_by": send_uid,
    "permission": perm_name,
    "title": "收到一篇分享笔记",
    "message": format!("【{}】将笔记「{}」分享给了你，赋予「{}」权限，请及时查看", sender_name, view_name, perm_name),
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
    "message": format!("你已将笔记「{}」分享给【{}】，已赋予「{}」权限", view_name, receiver_name, perm_name),
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
}

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
