use crate::biz::authentication::jwt::Authorization;
use crate::biz::subscription::ops::record_usage;
use crate::state::AppState;
use access_control::act::Action;
use actix_web::{web, HttpRequest, HttpResponse, Scope};
use ponynotes_weknora::{embedding, library, Client};
use serde_json::{json, Value};
use shared_entity::dto::subscription_dto::{UsageRecordRequest, UsageType};
use uuid::Uuid;

pub fn knowledge_scope() -> Scope {
  web::scope("/api/knowledge")
    .app_data(web::JsonConfig::default().limit(16384))
    .route("/{action}", web::post().to(knowledge))
}

pub fn knowledge_embedding_scope() -> Scope {
  // Docker-only route: public Nginx explicitly denies /internal/knowledge/.
  web::scope("/internal/knowledge")
    .app_data(web::JsonConfig::default().limit(8000000))
    .route("/{workspace_id}/v1/embeddings", web::post().to(embeddings))
}

fn failure(status: u16) -> HttpResponse {
  let message = match status {
    400 => "知识库参数错误。",
    401 => "登录已过期，请重新登录。",
    403 => "你没有此知识库的访问或管理权限。",
    404 => "知识库接口不存在。",
    409 => "该网页已存在于知识库中。",
    _ => "知识库暂时不可用，请稍后重试。",
  };
  HttpResponse::build(actix_web::http::StatusCode::from_u16(status).unwrap())
    .insert_header(("Cache-Control", "no-store"))
    .json(json!({"success":false,"message":message}))
}

async fn knowledge(
  auth: Authorization,
  action: web::Path<String>,
  payload: web::Json<Value>,
  state: web::Data<AppState>,
  client: web::Data<Client>,
) -> actix_web::Result<HttpResponse> {
  if !library::ACTIONS.contains(&action.as_str()) {
    return Ok(failure(404));
  }
  if !ponynotes_weknora::enabled() {
    return Ok(failure(503));
  }
  let uuid = auth.uuid()?;
  let uid = state.user_cache.get_user_uid(&uuid).await?;
  if action.as_str() != "bases" {
    let workspace = match library::uuid(&payload, "workspace_id") {
      Ok(id) => id,
      Err(_) => return Ok(failure(400)),
    };
    let permission = if matches!(
      action.as_str(),
      "create" | "import_url" | "web_import_options"
    ) {
      Action::Write
    } else {
      Action::Read
    };
    if state
      .workspace_access_control
      .enforce_action(&uid, &workspace, permission)
      .await
      .is_err()
    {
      return Ok(failure(403));
    }
  }
  let result = async {
    let session = library::Session::authenticate(&client, uuid, uid, &auth.token).await?;
    library::execute(
      &state.pg_pool,
      &client,
      &session,
      &auth.token,
      &action,
      &payload,
    )
    .await
  }
  .await;
  Ok(match result {
    Ok(data) => HttpResponse::Ok()
      .insert_header(("Cache-Control", "no-store"))
      .json(json!({"success":true,"data":data})),
    Err(error) => failure(library::status(&error)),
  })
}

async fn embeddings(
  request: HttpRequest,
  workspace: web::Path<Uuid>,
  payload: web::Json<Value>,
  state: web::Data<AppState>,
  client: web::Data<Client>,
) -> HttpResponse {
  if !ponynotes_weknora::enabled() {
    return failure(503);
  }
  let result: anyhow::Result<Value> = async {
    let proxy: Option<Value> = sqlx::query_scalar(
      "SELECT state->'workspaces'->$1->'embedding_proxy' FROM af_weknora_sync_state WHERE singleton",
    )
    .bind(workspace.to_string())
    .fetch_optional(&state.pg_pool)
    .await?
    .flatten();
    let proxy = proxy.unwrap_or(Value::Null);
    let authorization = request
      .headers()
      .get("Authorization")
      .and_then(|v| v.to_str().ok())
      .unwrap_or("");
    let inference = embedding::prepare(&proxy, authorization, &payload)?;
    // Snapshot the live owner before inference; transfers affect only future requests.
    let owner = embedding::owner(&state.pg_pool, *workspace).await?;
    let response = embedding::infer(&client, &inference).await?;
    let tokens = embedding::tokens(&response)?;
    if tokens > 0 {
      record_usage(
        &state.pg_pool,
        owner,
        UsageRecordRequest {
          usage_type: UsageType::AiTokens,
          usage_count: tokens,
          usage_date: None,
        },
      )
      .await?;
    }
    embedding::response(&proxy, &inference, &response, tokens)
  }
  .await;
  match result {
    Ok(data) => HttpResponse::Ok()
      .insert_header(("Cache-Control", "no-store"))
      .json(data),
    Err(error) => failure(library::status(&error)),
  }
}
