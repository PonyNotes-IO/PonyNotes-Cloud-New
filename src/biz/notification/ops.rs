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

/// 查询指定用户的未读通知（WebSocket 重连时补发）。
/// 以 is_read 而非 processed 作为过滤：processed 只表示"已投递过一次"，
/// 若客户端当时没接住(lag/断连)会被永久漏发；改按未读补发即可修复"有的收不到"。
pub async fn get_pending_notifications(
  pg_pool: &PgPool,
  recipient_uid: i64,
) -> Result<Vec<AFNotificationRow>, AppError> {
  use sqlx::Row;
  let rows = sqlx::query(
    r#"
    SELECT id, workspace_id, notification_type, payload, recipient_uid, created_at, processed
    FROM af_notification
    WHERE recipient_uid = $1 AND is_read = FALSE
    ORDER BY created_at ASC
    LIMIT 100
    "#,
  )
  .bind(recipient_uid)
  .fetch_all(pg_pool)
  .await
  .context("Query pending notifications")?;

  Ok(
    rows
      .iter()
      .map(|row| AFNotificationRow {
        id: row.get("id"),
        workspace_id: row.get("workspace_id"),
        notification_type: row.get("notification_type"),
        payload: row.get("payload"),
        recipient_uid: row.get("recipient_uid"),
        created_at: row.get("created_at"),
        processed: row.get("processed"),
      })
      .collect(),
  )
}

/// 标记单条通知为已读（仅限本人的通知）。返回受影响行数。
pub async fn mark_notification_read(
  pg_pool: &PgPool,
  recipient_uid: i64,
  notification_id: Uuid,
) -> Result<u64, AppError> {
  let result = sqlx::query(
    r#"
    UPDATE af_notification
    SET is_read = TRUE, read_at = now()
    WHERE id = $1 AND recipient_uid = $2 AND is_read = FALSE
    "#,
  )
  .bind(notification_id)
  .bind(recipient_uid)
  .execute(pg_pool)
  .await
  .context("Mark notification read")?;
  Ok(result.rows_affected())
}

/// 标记该用户全部未读通知为已读。返回受影响行数。
pub async fn mark_all_notifications_read(
  pg_pool: &PgPool,
  recipient_uid: i64,
) -> Result<u64, AppError> {
  let result = sqlx::query(
    r#"
    UPDATE af_notification
    SET is_read = TRUE, read_at = now()
    WHERE recipient_uid = $1 AND is_read = FALSE
    "#,
  )
  .bind(recipient_uid)
  .execute(pg_pool)
  .await
  .context("Mark all notifications read")?;
  Ok(result.rows_affected())
}

/// 设置单条通知的归档状态（仅限本人的通知）。返回受影响行数。
/// 归档状态收敛到服务端，保证任意工作区/设备看到一致的归档结果。
pub async fn set_notification_archived(
  pg_pool: &PgPool,
  recipient_uid: i64,
  notification_id: Uuid,
  archived: bool,
) -> Result<u64, AppError> {
  let result = sqlx::query(
    r#"
    UPDATE af_notification
    SET is_archived = $3,
        archived_at = CASE WHEN $3 THEN now() ELSE NULL END,
        is_read = CASE WHEN $3 THEN TRUE ELSE is_read END,
        read_at = CASE WHEN $3 AND read_at IS NULL THEN now() ELSE read_at END
    WHERE id = $1 AND recipient_uid = $2 AND is_archived <> $3
    "#,
  )
  .bind(notification_id)
  .bind(recipient_uid)
  .bind(archived)
  .execute(pg_pool)
  .await
  .context("Set notification archived")?;
  Ok(result.rows_affected())
}

/// 设置该用户全部通知的归档状态。归档时同时置已读（与客户端"归档即已读"语义一致）。
/// 返回受影响行数。
pub async fn set_all_notifications_archived(
  pg_pool: &PgPool,
  recipient_uid: i64,
  archived: bool,
) -> Result<u64, AppError> {
  let result = sqlx::query(
    r#"
    UPDATE af_notification
    SET is_archived = $2,
        archived_at = CASE WHEN $2 THEN now() ELSE NULL END,
        is_read = CASE WHEN $2 THEN TRUE ELSE is_read END,
        read_at = CASE WHEN $2 AND read_at IS NULL THEN now() ELSE read_at END
    WHERE recipient_uid = $1 AND is_archived <> $2
    "#,
  )
  .bind(recipient_uid)
  .bind(archived)
  .execute(pg_pool)
  .await
  .context("Set all notifications archived")?;
  Ok(result.rows_affected())
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


