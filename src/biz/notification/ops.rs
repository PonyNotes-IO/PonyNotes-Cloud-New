use anyhow::Context;
use app_error::AppError;
use database::pg_row::AFNotificationRow;
use sqlx::PgPool;
use uuid::Uuid;

pub async fn create_workspace_notification(
  pg_pool: &PgPool,
  workspace_id: &Uuid,
  notification_type: &str,
  payload_json: &serde_json::Value,
  recipient_uid: Option<i64>,
) -> Result<(), AppError> {
  tracing::info!(
    "[notification] inserting: type={}, workspace={}, recipient={:?}",
    notification_type, workspace_id, recipient_uid
  );
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

  tracing::info!(
    "[notification] inserted OK: type={}, recipient={:?}",
    notification_type, recipient_uid
  );
  Ok(())
}

/// 查询指定用户的未处理通知（在 WebSocket 重连时补发）
pub async fn get_pending_notifications(
  pg_pool: &PgPool,
  recipient_uid: i64,
) -> Result<Vec<AFNotificationRow>, AppError> {
  let rows = sqlx::query_as!(
    AFNotificationRow,
    r#"
    SELECT id, workspace_id, notification_type, payload, recipient_uid, created_at, processed
    FROM af_notification
    WHERE recipient_uid = $1 AND processed = FALSE
    ORDER BY created_at ASC
    "#,
    recipient_uid
  )
  .fetch_all(pg_pool)
  .await
  .context("Query pending notifications")?;

  Ok(rows)
}

/// 标记通知为已处理
pub async fn mark_notifications_processed(
  pg_pool: &PgPool,
  notification_ids: &[Uuid],
) -> Result<(), AppError> {
  if notification_ids.is_empty() {
    return Ok(());
  }
  sqlx::query!(
    r#"
    UPDATE af_notification
    SET processed = TRUE
    WHERE id = ANY($1)
    "#,
    notification_ids
  )
  .execute(pg_pool)
  .await
  .context("Mark notifications as processed")?;

  Ok(())
}


