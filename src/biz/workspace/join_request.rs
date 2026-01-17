use crate::biz::authentication::jwt::UserUuid;
use crate::state::AppState;
use actix_web::web::Data;
use app_error::AppError;
use database::user::select_uid_from_uuid;
use database_entity::dto::*;
use sqlx::types::uuid;
use tracing::instrument;
use uuid::Uuid;

/// Create a join request for a space
#[instrument(skip(state), err)]
pub async fn create_join_request(
  state: &Data<AppState>,
  user_uuid: &UserUuid,
  workspace_id: &Uuid,
  space_id: &Uuid,
  reason: Option<String>,
) -> Result<JoinRequest, AppError> {
  let uid = select_uid_from_uuid(&state.pg_pool, user_uuid).await?;
  let requester_id = uid as i64;

  // Check if user is already a member of the space
  let is_member = sqlx::query_scalar!(
    r#"
    SELECT EXISTS (
      SELECT 1 FROM af_collab_member
      WHERE oid = $1 AND uid = $2
    )
    "#,
    space_id.to_string(),
    uid
  )
  .fetch_one(&state.pg_pool)
  .await?;

  if is_member.unwrap_or(false) {
    return Err(AppError::RecordAlreadyExists("User is already a member of this space".to_string()));
  }

  // Check if there's already a pending request
  let existing_request = sqlx::query!(
    r#"
    SELECT id FROM join_requests
    WHERE workspace_id = $1 AND space_id = $2 AND requester_id = $3 AND status = 'pending'
    "#,
    workspace_id,
    space_id,
    requester_id
  )
  .fetch_optional(&state.pg_pool)
  .await?;

  if existing_request.is_some() {
    return Err(AppError::RecordAlreadyExists("Join request already exists".to_string()));
  }

  let now = chrono::Utc::now().timestamp();
  let request_id = uuid::Uuid::new_v4();

  let join_request = sqlx::query_as!(
    JoinRequest,
    r#"
    INSERT INTO join_requests (id, workspace_id, space_id, requester_id, reason, status, created_at, updated_at)
    VALUES ($1, $2, $3, $4, $5, 'pending', $6, $7)
    RETURNING id, workspace_id, space_id, requester_id, reason, status, created_at, updated_at
    "#,
    request_id,
    workspace_id,
    space_id,
    requester_id,
    reason,
    now,
    now
  )
  .fetch_one(&state.pg_pool)
  .await?;

  Ok(join_request)
}

/// List join requests for a space (space owner only)
#[instrument(skip(state), err)]
pub async fn list_join_requests(
  state: &Data<AppState>,
  user_uuid: &UserUuid,
  workspace_id: &Uuid,
  space_id: &Uuid,
) -> Result<Vec<JoinRequest>, AppError> {
  let uid = select_uid_from_uuid(&state.pg_pool, user_uuid).await?;

  // Check if user is the owner of the space
  let is_owner = sqlx::query_scalar!(
    r#"
    SELECT EXISTS (
      SELECT 1 FROM af_collab_member
      WHERE oid = $1 AND uid = $2 AND permission_id = (
        4
      )
    )
    "#,
    space_id.to_string(),
    uid
  )
  .fetch_one(&state.pg_pool)
  .await?;

  if !is_owner.unwrap_or(false) {
    return Err(AppError::NotEnoughPermissions);
  }

  let requests = sqlx::query_as!(
    JoinRequest,
    r#"
    SELECT id, workspace_id, space_id, requester_id, reason, status, created_at, updated_at
    FROM join_requests
    WHERE workspace_id = $1 AND space_id = $2
    ORDER BY created_at DESC
    "#,
    workspace_id,
    space_id
  )
  .fetch_all(&state.pg_pool)
  .await?;

  Ok(requests)
}

/// Handle join request (approve/reject) - space owner only
#[instrument(skip(state), err)]
pub async fn handle_join_request(
  state: &Data<AppState>,
  user_uuid: &UserUuid,
  workspace_id: &Uuid,
  space_id: &Uuid,
  request_id: &Uuid,
  approve: bool,
) -> Result<(), AppError> {
  let uid = select_uid_from_uuid(&state.pg_pool, user_uuid).await?;

  // Check if user is the owner of the space
  let is_owner = sqlx::query_scalar!(
    r#"
    SELECT EXISTS (
      SELECT 1 FROM af_collab_member
      WHERE oid = $1 AND uid = $2 AND permission_id = (
        4
      )
    )
    "#,
    space_id.to_string(),
    uid
  )
  .fetch_one(&state.pg_pool)
  .await?;

  if !is_owner.unwrap_or(false) {
    return Err(AppError::NotEnoughPermissions);
  }

  // Get the request and check if it exists
  let request = sqlx::query!(
    r#"
    SELECT requester_id, status FROM join_requests
    WHERE id = $1 AND workspace_id = $2 AND space_id = $3
    "#,
    request_id,
    workspace_id,
    space_id
  )
  .fetch_optional(&state.pg_pool)
  .await?
  .ok_or_else(|| AppError::RecordNotFound("Join request not found".to_string()))?;

  if request.status != "pending" {
    return Err(AppError::InvalidRequest("Request has already been processed".to_string()));
  }

  let now = chrono::Utc::now().timestamp();
  let status = if approve { "approved" } else { "rejected" };

  // Update the request status
  sqlx::query!(
    r#"
    UPDATE join_requests SET status = $1, updated_at = $2
    WHERE id = $3
    "#,
    status,
    now,
    request_id
  )
  .execute(&state.pg_pool)
  .await?;

  // If approved, add user to space
  if approve {
    // Use permission_id 3 which corresponds to 'Read and write' access for Member role
    sqlx::query!(
      r#"
      INSERT INTO af_collab_member (oid, uid, permission_id)
      VALUES ($1, $2, $3)
      ON CONFLICT (oid, uid) DO NOTHING
      "#,
      space_id.to_string(),
      request.requester_id,
      3i32
    )
    .execute(&state.pg_pool)
    .await?;
  }

  Ok(())
}

/// Cancel join request - requester only
#[instrument(skip(state), err)]
pub async fn cancel_join_request(
  state: &Data<AppState>,
  user_uuid: &UserUuid,
  workspace_id: &Uuid,
  space_id: &Uuid,
) -> Result<(), AppError> {
  let uid = select_uid_from_uuid(&state.pg_pool, user_uuid).await?;
  let requester_id = uid as i64;

  // Delete pending request for current user
  let result = sqlx::query!(
    r#"
    DELETE FROM join_requests
    WHERE workspace_id = $1 AND space_id = $2 AND requester_id = $3 AND status = 'pending'
    "#,
    workspace_id,
    space_id,
    requester_id
  )
  .execute(&state.pg_pool)
  .await?;

  if result.rows_affected() == 0 {
    return Err(AppError::RecordNotFound("No pending join request found".to_string()));
  }

  Ok(())
}
