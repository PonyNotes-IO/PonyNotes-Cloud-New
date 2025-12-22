use app_error::AppError;
use chrono::{DateTime, Utc};
use sqlx::{postgres::PgArguments, PgPool};
use tracing::instrument;
use serde_json::Value;

#[instrument(skip(pool), err)]
pub async fn upsert_user_cloud_integration(
  pool: &PgPool,
  user_uid: i64,
  provider: &str,
  access_token: Option<&str>,
  refresh_token: Option<&str>,
  expires_at: Option<DateTime<Utc>>,
  scopes: Option<&str>,
  meta: Option<Value>,
) -> Result<(), AppError> {
  // Upsert: insert or update existing record by user_uid + provider
  let q = r#"
  INSERT INTO user_cloud_integrations (provider, user_uid, access_token, refresh_token, expires_at, scopes, meta, is_revoked, last_refreshed_at)
  VALUES ($1, $2, $3, $4, $5, $6, $7, false, NOW())
  ON CONFLICT (user_uid, provider) DO UPDATE
  SET access_token = EXCLUDED.access_token,
      refresh_token = EXCLUDED.refresh_token,
      expires_at = EXCLUDED.expires_at,
      scopes = EXCLUDED.scopes,
      meta = EXCLUDED.meta,
      is_revoked = false,
      last_refreshed_at = NOW(),
      updated_at = NOW()
  "#;

  sqlx::query(q)
    .bind(provider)
    .bind(user_uid)
    .bind(access_token)
    .bind(refresh_token)
    .bind(expires_at)
    .bind(scopes)
    .bind(meta)
    .execute(pool)
    .await?;
  Ok(())
}

#[derive(Debug)]
pub struct UserCloudIntegrationRow {
  pub id: uuid::Uuid,
  pub provider: String,
  pub user_uid: i64,
  pub access_token: Option<String>,
  pub refresh_token: Option<String>,
  pub expires_at: Option<DateTime<Utc>>,
  pub scopes: Option<String>,
  pub meta: Option<Value>,
  pub is_revoked: bool,
  pub created_at: DateTime<Utc>,
  pub updated_at: DateTime<Utc>,
}

#[instrument(skip(pool), err)]
pub async fn select_user_cloud_integration(
  pool: &PgPool,
  user_uid: i64,
  provider: &str,
) -> Result<Option<UserCloudIntegrationRow>, AppError> {
  let row = sqlx::query!(
    r#"
    SELECT id, provider, user_uid, access_token, refresh_token, expires_at, scopes, meta, is_revoked, created_at, updated_at
    FROM user_cloud_integrations
    WHERE user_uid = $1 AND provider = $2
    "#,
    user_uid,
    provider
  )
  .fetch_optional(pool)
  .await?;

  if let Some(r) = row {
    Ok(Some(UserCloudIntegrationRow {
      id: r.id,
      provider: r.provider,
      user_uid: r.user_uid,
      access_token: r.access_token,
      refresh_token: r.refresh_token,
      expires_at: r.expires_at,
      scopes: r.scopes,
      meta: r.meta,
      is_revoked: r.is_revoked,
      created_at: r.created_at,
      updated_at: r.updated_at,
    }))
  } else {
    Ok(None)
  }
}


