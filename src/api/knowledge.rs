use crate::biz::authentication::jwt::Authorization;
use crate::state::AppState;
use access_control::act::Action;
use actix_web::{error, web, HttpResponse, Scope};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

#[derive(Deserialize)]
struct SearchRequest {
  workspace_id: Uuid,
  query: String,
}

pub fn knowledge_scope() -> Scope {
  web::scope("/api/knowledge").service(
    web::resource("/search")
      .app_data(web::JsonConfig::default().limit(16384))
      .route(web::post().to(search)),
  )
}

async fn search(
  auth: Authorization,
  payload: web::Json<SearchRequest>,
  state: web::Data<AppState>,
  client: web::Data<ponynotes_weknora::Client>,
) -> actix_web::Result<HttpResponse> {
  if !(1..=2000).contains(&payload.query.trim().chars().count()) {
    return Err(error::ErrorBadRequest("检索参数错误。"));
  }
  if !ponynotes_weknora::enabled() {
    return Err(error::ErrorServiceUnavailable("知识库检索尚未启用。"));
  }
  let uuid = auth.uuid()?;
  let uid = state.user_cache.get_user_uid(&uuid).await?;
  state
    .workspace_access_control
    .enforce_action(&uid, &payload.workspace_id, Action::Read)
    .await
    .map_err(|_| error::ErrorForbidden("无权检索此工作区。"))?;
  // Select only the authenticated user's mapping; never accept client tenant/KB IDs.
  let account: Option<Value> =
    sqlx::query_scalar("SELECT state->'users'->$1 FROM af_weknora_sync_state WHERE singleton")
      .bind(uuid.to_string())
      .fetch_optional(&state.pg_pool)
      .await
      .map_err(|_| error::ErrorServiceUnavailable("知识库同步尚未就绪。"))?
      .flatten();
  let result = ponynotes_weknora::search(
    &client,
    account.as_ref(),
    uuid,
    &auth.token,
    payload.workspace_id,
    &payload.query,
  )
  .await
  .map_err(|_| error::ErrorServiceUnavailable("知识库检索暂时不可用，请稍后重试。"))?;
  Ok(
    HttpResponse::Ok()
      .insert_header(("Cache-Control", "no-store"))
      .json(json!({"success":true,"data":result})),
  )
}
