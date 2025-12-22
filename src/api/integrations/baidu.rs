use actix_web::{web, HttpResponse, Scope};
use actix_web::web::Data;
use actix_web::Result;
use crate::state::AppState;
use crate::biz::authentication::jwt::Authorization;
use serde::Deserialize;
use shared_entity::response::{AppResponse, JsonAppResponse};
use serde_json::json;
use tracing::{instrument, event};
use infra::env_util::get_env_var;
use database::user;
use database::integrations as db_integrations;
use chrono::{DateTime, Utc};

pub fn baidu_scope() -> Scope {
  web::scope("/api/integrations/baidu")
    .service(web::resource("/authorize-url").route(web::get().to(get_authorize_url)))
    .service(web::resource("/exchange").route(web::post().to(post_exchange_code)))
    .service(web::resource("/migrate").route(web::post().to(post_migrate_tokens)))
    .service(web::resource("/files").route(web::get().to(get_files)))
    .service(web::resource("/download").route(web::get().to(get_download_url)))
}

#[derive(Deserialize)]
struct MigratePayload {
  access_token: Option<String>,
  refresh_token: Option<String>,
  expires_at: Option<String>, // ISO8601 string
  scopes: Option<String>,
  meta: Option<serde_json::Value>,
}

#[instrument(skip(state, payload, auth), err)]
async fn post_migrate_tokens(
  auth: Authorization,
  payload: web::Json<MigratePayload>,
  state: Data<AppState>,
) -> Result<JsonAppResponse<serde_json::Value>> {
  let user_uuid = auth.uuid()?;
  let uid = database::user::select_uid_from_uuid(&state.pg_pool, &user_uuid).await?;

  let p = payload.into_inner();
  let expires_at_parsed = p
    .expires_at
    .as_deref()
    .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
    .map(|dt| dt.with_timezone(&Utc));

  db_integrations::upsert_user_cloud_integration(
    &state.pg_pool,
    uid,
    "baidu",
    p.access_token.as_deref(),
    p.refresh_token.as_deref(),
    expires_at_parsed,
    p.scopes.as_deref(),
    p.meta,
  )
  .await?;

  Ok(AppResponse::Ok().with_data(json!({"ok": true})).into())
}

#[instrument(skip(state), err)]
async fn get_authorize_url(state: Data<AppState>) -> Result<JsonAppResponse<serde_json::Value>> {
  // Build authorize url from server-side env/config
  let client_id = get_env_var("BAIDU_CLOUD_APP_KEY", "");
  let redirect_uri = get_env_var("BAIDU_CLOUD_REDIRECT_URI", "http://localhost:8080/auth/callback");
  if client_id.is_empty() {
    return Ok(AppResponse::BadRequest("Baidu config not set on server".to_string()).into());
  }
  let params = [
    ("response_type", "code"),
    ("client_id", client_id.as_str()),
    ("redirect_uri", redirect_uri.as_str()),
    ("scope", "basic netdisk"),
    ("state", "baidu_cloud_auth"),
  ];
  let query: String = params
    .iter()
    .map(|(k, v)| format!("{}={}", urlencoding::encode(k), urlencoding::encode(v)))
    .collect::<Vec<_>>()
    .join("&");
  let url = format!("https://openapi.baidu.com/oauth/2.0/authorize?{}", query);
  Ok(AppResponse::Ok().with_data(json!({ "url": url })).into())
}

#[derive(Deserialize)]
struct ExchangePayload {
  code: String,
}

#[instrument(skip(state, payload), err)]
async fn post_exchange_code(
  auth: Authorization,
  payload: web::Json<ExchangePayload>,
  state: Data<AppState>,
) -> Result<JsonAppResponse<serde_json::Value>> {
  let user_uuid = auth.uuid()?;
  // map to internal uid
  let uid = database::user::select_uid_from_uuid(&state.pg_pool, &user_uuid).await?;

  let code = payload.code.clone();
  let client_id = get_env_var("BAIDU_CLOUD_APP_KEY", "");
  let client_secret = get_env_var("BAIDU_CLOUD_SECRET_KEY", "");
  let redirect_uri = get_env_var("BAIDU_CLOUD_REDIRECT_URI", "http://localhost:8080/auth/callback");
  if client_id.is_empty() || client_secret.is_empty() {
    return Ok(AppResponse::BadRequest("Baidu client secret not configured on server".to_string()).into());
  }

  // Exchange code for token
  let resp = reqwest::Client::new()
    .post("https://openapi.baidu.com/oauth/2.0/token")
    .form(&[
      ("grant_type", "authorization_code"),
      ("code", code.as_str()),
      ("client_id", client_id.as_str()),
      ("client_secret", client_secret.as_str()),
      ("redirect_uri", redirect_uri.as_str()),
    ])
    .send()
    .await
    .map_err(|e| {
      event!(tracing::Level::ERROR, "Failed to call baidu token endpoint: {:?}", e);
      actix_web::error::ErrorInternalServerError("token exchange failed")
    })?;

  let status = resp.status();
  let text = resp.text().await.unwrap_or_default();
  if !status.is_success() {
    event!(tracing::Level::ERROR, "baidu token exchange failed: {} {}", status, text);
    return Ok(AppResponse::InternalServerError("Baidu token exchange failed".to_string()).into());
  }
  let data: serde_json::Value = serde_json::from_str(&text).unwrap_or_else(|_| serde_json::json!({}));

  let access_token = data.get("access_token").and_then(|v| v.as_str()).map(|s| s.to_string());
  let refresh_token = data.get("refresh_token").and_then(|v| v.as_str()).map(|s| s.to_string());
  let expires_in = data.get("expires_in").and_then(|v| v.as_i64());
  let expires_at = expires_in.map(|s| Utc::now() + chrono::Duration::seconds(s));
  let scopes = data.get("scope").and_then(|v| v.as_str()).map(|s| s.to_string());

  db_integrations::upsert_user_cloud_integration(
    &state.pg_pool,
    uid,
    "baidu",
    access_token.as_deref(),
    refresh_token.as_deref(),
    expires_at,
    scopes.as_deref(),
    Some(data.clone()),
  )
  .await?;

  Ok(AppResponse::Ok().with_data(json!({"ok": true})).into())
}

#[instrument(skip(state, query), err)]
async fn get_files(
  auth: Authorization,
  query: web::Query<std::collections::HashMap<String, String>>,
  state: Data<AppState>,
) -> Result<HttpResponse> {
  let user_uuid = auth.uuid()?;
  let uid = database::user::select_uid_from_uuid(&state.pg_pool, &user_uuid).await?;
  let dir = query.get("dir").cloned().unwrap_or_else(|| "/".to_string());

  if let Some(row) = db_integrations::select_user_cloud_integration(&state.pg_pool, uid, "baidu").await? {
    if row.is_revoked {
      return Ok(HttpResponse::Forbidden().finish());
    }
    if let Some(token) = row.access_token {
      // call baidu list API
      let api = get_env_var("BAIDU_CLOUD_API_BASE", "https://pan.baidu.com/rest/2.0");
      let url = format!("{}/xpan/file?method=list&access_token={}", api, token);
      let resp = reqwest::Client::new()
        .post(&url)
        .form(&[("dir", dir.as_str())])
        .send()
        .await
        .map_err(|e| actix_web::error::ErrorInternalServerError(e))?;
      let status = resp.status();
      let text = resp.text().await.unwrap_or_default();
      if !status.is_success() {
        return Ok(HttpResponse::InternalServerError().body(text));
      }
      return Ok(HttpResponse::Ok().content_type("application/json").body(text));
    }
  }
  Ok(HttpResponse::NotFound().finish())
}

#[instrument(skip(state, query), err)]
async fn get_download_url(
  auth: Authorization,
  query: web::Query<std::collections::HashMap<String, String>>,
  state: Data<AppState>,
) -> Result<HttpResponse> {
  let user_uuid = auth.uuid()?;
  let uid = database::user::select_uid_from_uuid(&state.pg_pool, &user_uuid).await?;
  let fs_id = match query.get("fsId") {
    Some(v) => v.clone(),
    None => return Ok(HttpResponse::BadRequest().finish()),
  };

  if let Some(row) = db_integrations::select_user_cloud_integration(&state.pg_pool, uid, "baidu").await? {
    if row.is_revoked {
      return Ok(HttpResponse::Forbidden().finish());
    }
    if let Some(token) = row.access_token {
      let api = get_env_var("BAIDU_CLOUD_API_BASE", "https://pan.baidu.com/rest/2.0");
      let url = format!("{}/xpan/file?method=download&access_token={}", api, token);
      let resp = reqwest::Client::new()
        .post(&url)
        .form(&[("fidlist", format!("[{}]", fs_id).as_str())])
        .send()
        .await
        .map_err(|e| actix_web::error::ErrorInternalServerError(e))?;
      let status = resp.status();
      let text = resp.text().await.unwrap_or_default();
      if !status.is_success() {
        return Ok(HttpResponse::InternalServerError().body(text));
      }
      return Ok(HttpResponse::Ok().content_type("application/json").body(text));
    }
  }
  Ok(HttpResponse::NotFound().finish())
}


