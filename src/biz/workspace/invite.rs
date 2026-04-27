use app_error::AppError;
use database::workspace::{
  delete_all_invite_code_for_workspace, insert_workspace_invite_code, select_invitation_code_info,
  select_invite_code_for_workspace_id, select_invited_workspace_id, upsert_workspace_member_uid,
};
use rand::{distributions::Alphanumeric, Rng};
use sqlx::PgPool;
use uuid::Uuid;

use database_entity::dto::{AFRole, InvitationCodeInfo, WorkspaceInviteToken};
use crate::biz::notification::ops::create_workspace_notification;

const INVITE_LINK_CODE_LENGTH: usize = 16;

pub async fn generate_workspace_invite_token(
  pg_pool: &PgPool,
  workspace_id: &Uuid,
  validity_period_hours: Option<i64>,
) -> Result<WorkspaceInviteToken, AppError> {
  delete_all_invite_code_for_workspace(pg_pool, workspace_id).await?;
  let code = generate_workspace_invite_code();
  let expires_at = validity_period_hours.map(|v| chrono::Utc::now() + chrono::Duration::hours(v));
  insert_workspace_invite_code(pg_pool, workspace_id, &code, expires_at.as_ref()).await?;

  // 创建通知：工作空间所有者收到"已生成邀请链接"的通知
  let payload = serde_json::json!({
    "invite_code": code,
    "expires_at": expires_at,
    "created_at": chrono::Utc::now().timestamp(),
    "message": "您的工作空间邀请链接已生成"
  });
  if let Err(err) = create_workspace_notification(pg_pool, workspace_id, "workspace_invite_created", &payload, None).await {
    tracing::warn!("Failed to create workspace invite notification: {:?}", err);
  }

  Ok(WorkspaceInviteToken { code: Some(code) })
}

fn generate_workspace_invite_code() -> String {
  let rng = rand::thread_rng();
  rng
    .sample_iter(&Alphanumeric)
    .take(INVITE_LINK_CODE_LENGTH)
    .map(char::from)
    .collect()
}

pub async fn join_workspace_invite_by_code(
  pg_pool: &PgPool,
  invitation_code: &str,
  uid: i64,
) -> Result<Uuid, AppError> {
  let invited_workspace_id = select_invited_workspace_id(pg_pool, invitation_code).await?;
  upsert_workspace_member_uid(pg_pool, &invited_workspace_id, uid, AFRole::Member).await?;

  // 获取工作区名称和新成员名称
  let workspace_name = database::workspace::select_workspace_name_from_workspace_id(pg_pool, &invited_workspace_id)
    .await
    .unwrap_or(None)
    .unwrap_or_else(|| "工作区".to_string());
  let joiner_name = database::user::select_name_from_uid(pg_pool, uid)
    .await
    .unwrap_or_else(|_| "新成员".to_string());

  // 通知工作区所有者（A）：某人通过邀请链接加入
  let payload_owner = serde_json::json!({
    "invite_code": invitation_code,
    "joined_uid": uid,
    "joined_name": joiner_name,
    "joined_at": chrono::Utc::now().timestamp(),
    "workspace_name": workspace_name,
    "role": "成员",
    "title": "新成员加入工作区",
    "message": format!("【{}】通过邀请链接加入了你的工作区「{}」", joiner_name, workspace_name),
  });
  if let Err(err) = create_workspace_notification(pg_pool, &invited_workspace_id, "workspace_member_joined", &payload_owner, None).await {
    tracing::warn!("Failed to create workspace member joined notification: {:?}", err);
  }

  // 通知新成员（B）：加入成功确认
  let payload_joiner = serde_json::json!({
    "workspace_id": invited_workspace_id.to_string(),
    "workspace_name": workspace_name,
    "role": "成员",
    "title": "成功加入工作区",
    "message": format!("你已通过邀请链接成功加入工作区「{}」，角色：成员", workspace_name),
  });
  if let Err(err) = create_workspace_notification(pg_pool, &invited_workspace_id, "workspace_joined_via_code", &payload_joiner, Some(uid)).await {
    tracing::warn!("Failed to create workspace joined via code notification to uid={}: {:?}", uid, err);
  }

  Ok(invited_workspace_id)
}

pub async fn delete_workspace_invite_code(
  pg_pool: &PgPool,
  workspace_id: &Uuid,
) -> Result<(), AppError> {
  delete_all_invite_code_for_workspace(pg_pool, workspace_id).await?;
  Ok(())
}

pub async fn get_invite_code_for_workspace(
  pg_pool: &PgPool,
  workspace_id: &Uuid,
) -> Result<Option<String>, AppError> {
  let code = select_invite_code_for_workspace_id(pg_pool, workspace_id).await?;
  Ok(code)
}

pub async fn get_invitation_code_info(
  pg_pool: &PgPool,
  invitation_code: &str,
  uid: i64,
) -> Result<InvitationCodeInfo, AppError> {
  let info_list = select_invitation_code_info(pg_pool, invitation_code, uid).await?;
  info_list
    .into_iter()
    .next()
    .ok_or(AppError::InvalidInvitationCode)
}
