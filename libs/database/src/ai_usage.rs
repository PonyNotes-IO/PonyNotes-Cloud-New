use app_error::AppError;
use chrono::{Datelike, Utc};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

/// Get the total AI responses used in the current month for a workspace
pub async fn get_workspace_ai_usage_this_month(
  pool: &PgPool,
  workspace_id: &Uuid,
) -> Result<i64, AppError> {
  let now = Utc::now();
  let year = now.year();
  let month = now.month();
  
  // Get the first and last day of the current month
  let first_day = chrono::NaiveDate::from_ymd_opt(year, month, 1)
    .ok_or_else(|| AppError::Internal(anyhow::anyhow!("Invalid date")))?;
  
  let last_day = if month == 12 {
    chrono::NaiveDate::from_ymd_opt(year + 1, 1, 1)
      .ok_or_else(|| AppError::Internal(anyhow::anyhow!("Invalid date")))?
  } else {
    chrono::NaiveDate::from_ymd_opt(year, month + 1, 1)
      .ok_or_else(|| AppError::Internal(anyhow::anyhow!("Invalid date")))?
  };
  
  let result = sqlx::query_scalar!(
    r#"
      SELECT COALESCE(SUM(ai_responses_count), 0)::bigint as "total!"
      FROM af_workspace_ai_usage
      WHERE workspace_id = $1
        AND created_at >= $2
        AND created_at < $3
    "#,
    workspace_id,
    first_day,
    last_day
  )
  .fetch_one(pool)
  .await?;
  
  Ok(result)
}

/// Increment the AI response count for today
pub async fn increment_ai_usage(
  pool: &PgPool,
  workspace_id: &Uuid,
) -> Result<(), AppError> {
  let today = Utc::now().date_naive();
  
  sqlx::query!(
    r#"
      INSERT INTO af_workspace_ai_usage (created_at, workspace_id, ai_responses_count)
      VALUES ($1, $2, 1)
      ON CONFLICT (created_at, workspace_id)
      DO UPDATE SET ai_responses_count = af_workspace_ai_usage.ai_responses_count + 1
    "#,
    today,
    workspace_id
  )
  .execute(pool)
  .await?;
  
  Ok(())
}

/// Increment the AI response count within a transaction
pub async fn increment_ai_usage_txn(
  txn: &mut Transaction<'_, Postgres>,
  workspace_id: &Uuid,
) -> Result<(), AppError> {
  let today = Utc::now().date_naive();
  
  sqlx::query!(
    r#"
      INSERT INTO af_workspace_ai_usage (created_at, workspace_id, ai_responses_count)
      VALUES ($1, $2, 1)
      ON CONFLICT (created_at, workspace_id)
      DO UPDATE SET ai_responses_count = af_workspace_ai_usage.ai_responses_count + 1
    "#,
    today,
    workspace_id
  )
  .execute(&mut **txn)
  .await?;
  
  Ok(())
}

/// Get the total AI image responses used in the current month for a workspace
pub async fn get_workspace_ai_image_usage_this_month(
  pool: &PgPool,
  workspace_id: &Uuid,
) -> Result<i64, AppError> {
  let now = Utc::now();
  let year = now.year();
  let month = now.month();
  
  let first_day = chrono::NaiveDate::from_ymd_opt(year, month, 1)
    .ok_or_else(|| AppError::Internal(anyhow::anyhow!("Invalid date")))?;
  
  let last_day = if month == 12 {
    chrono::NaiveDate::from_ymd_opt(year + 1, 1, 1)
      .ok_or_else(|| AppError::Internal(anyhow::anyhow!("Invalid date")))?
  } else {
    chrono::NaiveDate::from_ymd_opt(year, month + 1, 1)
      .ok_or_else(|| AppError::Internal(anyhow::anyhow!("Invalid date")))?
  };
  
  let result = sqlx::query_scalar!(
    r#"
      SELECT COALESCE(SUM(ai_image_responses_count), 0)::bigint as "total!"
      FROM af_workspace_ai_usage
      WHERE workspace_id = $1
        AND created_at >= $2
        AND created_at < $3
    "#,
    workspace_id,
    first_day,
    last_day
  )
  .fetch_one(pool)
  .await?;
  
  Ok(result)
}

/// Increment the AI image response count for today
pub async fn increment_ai_image_usage(
  pool: &PgPool,
  workspace_id: &Uuid,
) -> Result<(), AppError> {
  let today = Utc::now().date_naive();
  
  sqlx::query!(
    r#"
      INSERT INTO af_workspace_ai_usage (created_at, workspace_id, ai_image_responses_count)
      VALUES ($1, $2, 1)
      ON CONFLICT (created_at, workspace_id)
      DO UPDATE SET ai_image_responses_count = af_workspace_ai_usage.ai_image_responses_count + 1
    "#,
    today,
    workspace_id
  )
  .execute(pool)
  .await?;
  
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;
  
  // Note: These tests require a running PostgreSQL database
  // They are integration tests and should be run with `cargo test --test '*'`
  
  #[tokio::test]
  #[ignore] // Ignore by default as it requires a database
  async fn test_increment_and_get_ai_usage() {
    // This test would need a test database setup
    // Implementation depends on your test infrastructure
  }
}

