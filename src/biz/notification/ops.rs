use anyhow::Context;
use app_error::AppError;
use sqlx::PgPool;
use uuid::Uuid;

pub async fn create_workspace_notification(
  pg_pool: &PgPool,
  workspace_id: &Uuid,
  notification_type: &str,
  payload_json: &serde_json::Value,
  recipient_uid: Option<i64>,
) -> Result<(), AppError> {
  // Insert a notification row; the DB trigger will emit a pg_notify so realtime workers / listeners can pick it up.
  sqlx::query!(
    r#"
    INSERT INTO af_notification (workspace_id, notification_type, payload, recipient_uid)
    VALUES ($1, $2, $3, $4)
    "#,
    workspace_id,
    notification_type,
    payload_json,
    recipient_uid
  )
  .execute(pg_pool)
  .await
  .context("Insert notification row")?;

  Ok(())
}


