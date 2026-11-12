//! Cloud owns identity, note scope and billing; WeKnora owns the agent loop.
use super::{gateway, issue_session_token};
use crate::{biz::authentication::jwt::Authorization, state::AppState};
use access_control::act::Action;
use actix_web::{error, web, HttpResponse, Scope};
use futures_util::{stream, StreamExt};
use ponynotes_weknora::{agent, Client};
use redis::AsyncCommands;
use reqwest::Method;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

pub(super) type Document = web::Path<(Uuid, Uuid)>;

pub fn scope() -> Scope {
  web::scope("/conversations/{workspace_id}/{document_id}")
    .app_data(web::JsonConfig::default().limit(30 * 1024 * 1024))
    .route("", web::get().to(history))
    .route("/messages", web::post().to(send))
    .route("/stop", web::post().to(stop))
    .route("/steer", web::post().to(steer))
    .route("/stream", web::get().to(resume))
    .route("/tools", web::post().to(super::note_tools::register))
    .route("/tools", web::get().to(super::note_tools::poll))
    .route("/tools", web::delete().to(super::note_tools::revoke))
    .route("/tool-result", web::post().to(super::note_tools::result))
}

pub(super) fn unavailable(_: impl std::fmt::Display) -> actix_web::Error {
  error::ErrorServiceUnavailable("小马 Agent服务暂不可用，请稍后重试；请勿重复提交已接受的问题")
}

struct Context {
  uid: i64,
  workspace: Uuid,
  document: Uuid,
  scope: agent::Scope,
}

async fn context(
  auth: &Authorization,
  path: &Document,
  state: &AppState,
  client: &Client,
) -> actix_web::Result<Context> {
  if !ponynotes_weknora::enabled() {
    return Err(error::ErrorServiceUnavailable("小马 Agent尚未启用"));
  }
  let uuid = auth.uuid()?;
  let uid = state.user_cache.get_user_uid(&uuid).await?;
  let (workspace, document) = **path;
  state
    .workspace_access_control
    .enforce_action(&uid, &workspace, Action::Read)
    .await
    .map_err(|_| error::ErrorForbidden("无权访问此工作区"))?;
  let account: Option<Value> =
    sqlx::query_scalar("SELECT state->'users'->$1 FROM af_weknora_sync_state WHERE singleton")
      .bind(uuid.to_string())
      .fetch_optional(&state.pg_pool)
      .await
      .map_err(unavailable)?
      .flatten();
  let account =
    account.ok_or_else(|| error::ErrorServiceUnavailable("知识库账户尚未同步，请稍后重试"))?;
  let scope = client
    .agent_scope(&account, uuid, &auth.token, workspace, document)
    .await
    .map_err(unavailable)?;
  Ok(Context {
    uid,
    workspace,
    document,
    scope,
  })
}

async fn stored(ctx: &Context, state: &AppState) -> actix_web::Result<Value> {
  Ok(sqlx::query_scalar("SELECT state FROM af_weknora_agent_sessions WHERE uid=$1 AND workspace_id=$2 AND document_id=$3")
    .bind(ctx.uid).bind(ctx.workspace).bind(ctx.document).fetch_optional(&state.pg_pool).await.map_err(unavailable)?.unwrap_or(json!({})))
}

async fn save(
  ctx: &Context,
  record: &Value,
  connection: &mut sqlx::PgConnection,
) -> actix_web::Result<()> {
  sqlx::query("UPDATE af_weknora_agent_sessions SET state=$4 WHERE uid=$1 AND workspace_id=$2 AND document_id=$3")
    .bind(ctx.uid).bind(ctx.workspace).bind(ctx.document).bind(record).execute(connection).await.map_err(unavailable)?;
  Ok(())
}

fn session_id(record: &Value) -> actix_web::Result<&str> {
  record["session_id"]
    .as_str()
    .ok_or_else(|| error::ErrorNotFound("此文档还没有小马 Agent会话"))
}

fn scope_ids(ctx: &Context) -> Value {
  json!(ctx.scope.documents.keys().collect::<Vec<_>>())
}
fn scope_matches(ctx: &Context, record: &Value) -> bool {
  record["scope_ids"].as_array().is_some_and(|ids| {
    ids.iter().all(|id| {
      id.as_str()
        .is_some_and(|id| ctx.scope.documents.contains_key(id))
    })
  })
}
fn active_message(messages: &[Value]) -> Option<&Value> {
  messages
    .iter()
    .rev()
    .find(|m| m["role"] == "assistant")
    .filter(|m| m["is_completed"] == false)
}

async fn history(
  auth: Authorization,
  path: Document,
  state: web::Data<AppState>,
  client: web::Data<Client>,
) -> actix_web::Result<HttpResponse> {
  let ctx = context(&auth, &path, &state, &client).await?;
  let record = stored(&ctx, &state).await?;
  let changed = record["session_id"].is_string() && !scope_matches(&ctx, &record);
  let messages = if !changed {
    if let Some(id) = record["session_id"].as_str() {
      client
        .agent_history(&ctx.scope.token, id)
        .await
        .map_err(unavailable)?
    } else {
      vec![]
    }
  } else {
    vec![]
  };
  Ok(
    HttpResponse::Ok()
      .insert_header(("Cache-Control", "no-store"))
      .json(json!({
    "messages":messages,"sources":ctx.scope.documents,"scope_changed":changed,
    "active_message_id":active_message(&messages).and_then(|m|m["id"].as_str())})),
  )
}

#[derive(Deserialize)]
struct Model {
  model_name: String,
  #[serde(default)]
  is_internal: bool,
  #[serde(default)]
  base_url: String,
  #[serde(default)]
  api_key: String,
  #[serde(default)]
  supports_reasoning: bool,
}

#[derive(Deserialize)]
struct Send {
  query: String,
  model: Model,
  #[serde(default)]
  images: Vec<Value>,
  #[serde(default)]
  reasoning_effort: String,
  #[serde(default)]
  web_search: bool,
  #[serde(default)]
  initial_request: bool,
  client_id: Uuid,
}

fn validate(payload: &Send) -> actix_web::Result<()> {
  if payload.query.trim().is_empty()
    || payload.query.chars().count() > 100_000
    || payload.model.model_name.trim().is_empty()
    || payload.model.model_name.len() > 256
    || payload.images.len() > 4
  {
    return Err(error::ErrorBadRequest("问题、模型或附件参数无效"));
  }
  if !["", "off", "minimal", "low", "medium", "high"].contains(&payload.reasoning_effort.as_str()) {
    return Err(error::ErrorBadRequest("思考程度无效"));
  }
  for image in &payload.images {
    let data = image["data"].as_str().unwrap_or("");
    if ![
      "data:image/png;base64,",
      "data:image/jpeg;base64,",
      "data:image/webp;base64,",
      "data:image/gif;base64,",
    ]
    .iter()
    .any(|prefix| data.starts_with(prefix))
      || data.len() > 7 * 1024 * 1024
    {
      return Err(error::ErrorBadRequest("图片格式无效或超过 5 MB"));
    }
  }
  if !payload.model.is_internal {
    let url = reqwest::Url::parse(&payload.model.base_url)
      .map_err(|_| error::ErrorBadRequest("自定义模型地址无效"))?;
    if url.scheme() != "https"
      || url.host_str().is_none()
      || !url.username().is_empty()
      || url.password().is_some()
      || url.query().is_some()
      || url.fragment().is_some()
      || payload.model.api_key.trim().is_empty()
    {
      return Err(error::ErrorBadRequest(
        "小马 Agent的自定义模型需要服务端可访问的 HTTPS 地址和 API Key",
      ));
    }
  }
  Ok(())
}

async fn send(
  auth: Authorization,
  path: Document,
  state: web::Data<AppState>,
  client: web::Data<Client>,
  payload: web::Json<Send>,
) -> actix_web::Result<HttpResponse> {
  validate(&payload)?;
  let ctx = context(&auth, &path, &state, &client).await?;
  let note_channel = super::note_tools::channel(
    &state,
    ctx.uid,
    ctx.workspace,
    ctx.document,
    payload.client_id,
  )
  .await?;
  sqlx::query("INSERT INTO af_weknora_agent_sessions(uid,workspace_id,document_id) VALUES($1,$2,$3) ON CONFLICT DO NOTHING")
    .bind(ctx.uid).bind(ctx.workspace).bind(ctx.document).execute(&state.pg_pool).await.map_err(unavailable)?;
  let mut connection = state.pg_pool.acquire().await.map_err(unavailable)?;
  // Closing releases the session lock even if this request is cancelled mid-await.
  connection.close_on_drop();
  let locked: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock(hashtextextended($1, 0))")
    .bind(format!(
      "weknora-agent:{}:{}:{}",
      ctx.uid, ctx.workspace, ctx.document
    ))
    .fetch_one(&mut *connection)
    .await
    .map_err(unavailable)?;
  if !locked {
    return Err(error::ErrorConflict("此对话正在提交问题，请稍后重试"));
  }
  let mut record:Value=sqlx::query("SELECT state FROM af_weknora_agent_sessions WHERE uid=$1 AND workspace_id=$2 AND document_id=$3")
    .bind(ctx.uid).bind(ctx.workspace).bind(ctx.document).fetch_one(&mut *connection).await.map_err(unavailable)?.get("state");
  if record["mcp_id"].is_null() {
    let services = client
      .agent_request(&ctx.scope.token, "mcp-services", Method::GET, None)
      .await
      .map_err(unavailable)?;
    if let Some(existing) = services["data"].as_array().and_then(|items| {
      items
        .iter()
        .find(|item| item["name"] == format!("小马笔记工具-{}", ctx.document))
    }) {
      record["mcp_id"] = existing["id"].clone();
    }
  }
  if let Some(id) = record["session_id"].as_str() {
    let messages = client
      .agent_history(&ctx.scope.token, id)
      .await
      .map_err(unavailable)?;
    if active_message(&messages).is_some() {
      return Err(error::ErrorConflict("当前回答尚未结束，请先停止或恢复连接"));
    }
    if payload.initial_request && !messages.is_empty() {
      return Err(error::ErrorConflict(
        "初始问题已提交，请重新打开文档恢复回答",
      ));
    }
  }
  let base;
  let credential;
  if payload.model.is_internal {
    let gw = gateway(&state)?;
    let key = gw.key_for_user(&state, ctx.uid).await?;
    let available = gw.get("models", &key).await?;
    if !available["data"]
      .as_array()
      .is_some_and(|models| models.iter().any(|m| m["id"] == payload.model.model_name))
    {
      return Err(error::ErrorBadRequest("所选内部模型已不可用"));
    }
    base = client.model_base_url();
    super::revoke_conversation_model(&state, ctx.uid, ctx.workspace, ctx.document).await?;
    credential = issue_session_token(&state, ctx.uid).await?;
    let _: () = state
      .redis_connection_manager
      .clone()
      .set_ex(
        super::conversation_model_key(ctx.uid, ctx.workspace, ctx.document),
        &credential,
        super::SESSION_SECONDS,
      )
      .await
      .map_err(unavailable)?;
  } else {
    base = payload
      .model
      .base_url
      .trim_end_matches('/')
      .trim_end_matches("/chat/completions")
      .to_owned();
    credential = payload.model.api_key.clone();
  }
  let model_payload = json!({"name":payload.model.model_name,"display_name":format!("小马笔记 · {}",ctx.document),"type":"KnowledgeQA","source":"remote",
    "parameters":{"base_url":base,"api_key":credential,"provider":"generic","interface_type":"openai","supports_vision":true,"context_window":128000,"max_output_tokens":8192,"spec":{"reasoning":payload.model.supports_reasoning,"context_window":128000,"max_output_tokens":8192}}});
  let model_id = if let Some(id) = record["model_id"].as_str() {
    client
      .agent_request(
        &ctx.scope.token,
        &format!("models/{id}"),
        Method::PUT,
        Some(&model_payload),
      )
      .await
      .map_err(unavailable)?;
    client
      .agent_request(
        &ctx.scope.token,
        &format!("models/{id}/credentials"),
        Method::PUT,
        Some(&json!({"api_key":credential})),
      )
      .await
      .map_err(unavailable)?;
    id.to_owned()
  } else {
    let created = client
      .agent_request(
        &ctx.scope.token,
        "models",
        Method::POST,
        Some(&model_payload),
      )
      .await
      .map_err(unavailable)?;
    created["data"]["id"]
      .as_str()
      .ok_or_else(|| unavailable("missing model"))?
      .to_owned()
  };
  record["model_id"] = json!(model_id);
  save(&ctx, &record, &mut connection).await?;
  let mcp_base = client.model_base_url();
  let mcp_payload = json!({"name":format!("小马笔记工具-{}",ctx.document),"description":"小马 Agent 的笔记与数据库操作；客户端在执行边界检查权限并审批写入。","usage_instructions":format!("当前工作区 ID：{}。当前问 AI 文档 ID：{}。笔记工具覆盖查找、读取、创建和编辑笔记、块与数据库；操作当前工作区，页面 ID 先查询。",ctx.workspace,ctx.document),"enabled":true,"transport_type":"http-streamable","url":format!("{}/note-tools",mcp_base.trim_end_matches('/')),"auth_config":{"auth_type":"bearer","token":note_channel},"advanced_config":{"timeout":150,"retry_count":0,"retry_delay":1}});
  let mcp_id = if let Some(id) = record["mcp_id"].as_str() {
    client
      .agent_request(
        &ctx.scope.token,
        &format!("mcp-services/{id}"),
        Method::PUT,
        Some(&mcp_payload),
      )
      .await
      .map_err(unavailable)?;
    client
      .agent_request(
        &ctx.scope.token,
        &format!("mcp-services/{id}/credentials"),
        Method::PUT,
        Some(&json!({"token":note_channel})),
      )
      .await
      .map_err(unavailable)?;
    id.to_owned()
  } else {
    let created = client
      .agent_request(
        &ctx.scope.token,
        "mcp-services",
        Method::POST,
        Some(&mcp_payload),
      )
      .await
      .map_err(unavailable)?;
    created["data"]["id"]
      .as_str()
      .ok_or_else(|| unavailable("missing note service"))?
      .to_owned()
  };
  record["mcp_id"] = json!(mcp_id);
  save(&ctx, &record, &mut connection).await?;
  client
    .agent_request(
      &ctx.scope.token,
      &format!("mcp-services/{mcp_id}/metadata/refresh"),
      Method::POST,
      Some(&json!({})),
    )
    .await
    .map_err(unavailable)?;
  let mut config = agent::config(
    &model_id,
    !ctx.scope.documents.is_empty(),
    payload.web_search,
  );
  config["mcp_selection_mode"] = json!("selected");
  config["mcp_services"] = json!([mcp_id]);
  let agent_id = if let Some(id) = record["agent_id"].as_str() {
    client
      .agent_request(
        &ctx.scope.token,
        &format!("agents/{id}"),
        Method::PUT,
        Some(&json!({"name":"小马 Agent","config":config})),
      )
      .await
      .map_err(unavailable)?;
    id.to_owned()
  } else {
    let created = client
      .agent_request(
        &ctx.scope.token,
        "agents",
        Method::POST,
        Some(&json!({"name":"小马 Agent","config":config})),
      )
      .await
      .map_err(unavailable)?;
    created["data"]["id"]
      .as_str()
      .ok_or_else(|| unavailable("missing agent"))?
      .to_owned()
  };
  record["agent_id"] = json!(agent_id);
  save(&ctx, &record, &mut connection).await?;
  if !scope_matches(&ctx, &record) || record["session_id"].is_null() {
    if let Some(previous) = record["session_id"].as_str() {
      let mut archived = record["previous_sessions"]
        .as_array()
        .cloned()
        .unwrap_or_default();
      archived.push(json!(previous));
      record["previous_sessions"] = json!(archived);
    }
    let created = client
      .agent_request(
        &ctx.scope.token,
        "sessions",
        Method::POST,
        Some(&json!({"title":"小马笔记知识对话"})),
      )
      .await
      .map_err(unavailable)?;
    record["session_id"] = created["data"]["id"].clone();
  }
  record["scope_ids"] = scope_ids(&ctx);
  let session = session_id(&record)?.to_owned();
  save(&ctx, &record, &mut connection).await?;
  let request = json!({"query":payload.query,"agent_id":agent_id,"agent_enabled":true,"knowledge_ids":ctx.scope.documents.keys().collect::<Vec<_>>(),
    "knowledge_base_ids":[],"images":payload.images,"reasoning_effort":if payload.model.supports_reasoning {payload.reasoning_effort.as_str()}else{"off"},"web_search_enabled":payload.web_search,"channel":"api"});
  let upstream = client
    .agent_stream(
      &ctx.scope.token,
      &format!("agent-chat/{session}"),
      Method::POST,
      Some(&request),
    )
    .await
    .map_err(unavailable)?;
  connection.close().await.map_err(unavailable)?;
  Ok(stream_response(
    upstream,
    json!({"response_type":"scope","data":{"sources":ctx.scope.documents}}),
  ))
}

fn stream_response(upstream: reqwest::Response, metadata: Value) -> HttpResponse {
  let first = web::Bytes::from(format!("data: {metadata}\n\n"));
  HttpResponse::Ok()
    .content_type("text/event-stream")
    .insert_header(("Cache-Control", "no-store"))
    .insert_header(("X-Accel-Buffering", "no"))
    .streaming(
      stream::once(async move { Ok::<_, actix_web::Error>(first) }).chain(
        upstream
          .bytes_stream()
          .map(|chunk| chunk.map_err(unavailable)),
      ),
    )
}

async fn stop(
  auth: Authorization,
  path: Document,
  state: web::Data<AppState>,
  client: web::Data<Client>,
) -> actix_web::Result<HttpResponse> {
  let uid = super::note_tools::authorize(&auth, &path, &state).await?;
  super::note_tools::cancel(&state, uid, path.as_ref().0, path.as_ref().1).await?;
  let ctx = context(&auth, &path, &state, &client).await?;
  let record = stored(&ctx, &state).await?;
  client
    .agent_request(
      &ctx.scope.token,
      &format!("sessions/{}/stop", session_id(&record)?),
      Method::POST,
      Some(&json!({})),
    )
    .await
    .map_err(unavailable)?;
  Ok(HttpResponse::NoContent().finish())
}

#[derive(Deserialize)]
struct Steer {
  query: String,
  #[serde(default)]
  delivery: String,
  expected_assistant_message_id: Uuid,
}
async fn steer(
  auth: Authorization,
  path: Document,
  state: web::Data<AppState>,
  client: web::Data<Client>,
  payload: web::Json<Steer>,
) -> actix_web::Result<HttpResponse> {
  if payload.query.trim().is_empty()
    || payload.query.chars().count() > 100_000
    || !["inject", "after"].contains(&payload.delivery.as_str())
  {
    return Err(error::ErrorBadRequest("补充指令无效"));
  }
  let ctx = context(&auth, &path, &state, &client).await?;
  let record = stored(&ctx, &state).await?;
  if !scope_matches(&ctx, &record) {
    return Err(error::ErrorConflict("笔记授权范围已变化，请停止后重新提问"));
  }
  let body=client.agent_request(&ctx.scope.token,&format!("sessions/{}/steer",session_id(&record)?),Method::POST,Some(&json!({"query":payload.query,"delivery":payload.delivery,"expected_assistant_message_id":payload.expected_assistant_message_id}))).await.map_err(unavailable)?;
  Ok(HttpResponse::Ok().json(body))
}

#[derive(Deserialize)]
struct Resume {
  message_id: Uuid,
}
async fn resume(
  auth: Authorization,
  path: Document,
  state: web::Data<AppState>,
  client: web::Data<Client>,
  query: web::Query<Resume>,
) -> actix_web::Result<HttpResponse> {
  let ctx = context(&auth, &path, &state, &client).await?;
  let record = stored(&ctx, &state).await?;
  if !scope_matches(&ctx, &record) {
    return Err(error::ErrorConflict("笔记授权范围已变化，请停止后重新提问"));
  }
  let messages = client
    .agent_history(&ctx.scope.token, session_id(&record)?)
    .await
    .map_err(unavailable)?;
  if !messages
    .iter()
    .any(|m| m["id"] == query.message_id.to_string() && m["role"] == "assistant")
  {
    return Err(error::ErrorNotFound("回答不存在"));
  }
  let upstream = client
    .agent_stream(
      &ctx.scope.token,
      &format!(
        "sessions/continue-stream/{}?message_id={}",
        session_id(&record)?,
        query.message_id
      ),
      Method::GET,
      None,
    )
    .await
    .map_err(unavailable)?;
  Ok(stream_response(
    upstream,
    json!({"response_type":"scope","data":{"sources":ctx.scope.documents}}),
  ))
}

#[cfg(test)]
mod tests {
  use super::*;
  #[test]
  fn validates_model_and_image_boundaries() {
    let mut p = Send {
      query: "问题".into(),
      model: Model {
        model_name: "m".into(),
        is_internal: true,
        base_url: String::new(),
        api_key: String::new(),
        supports_reasoning: false,
      },
      images: vec![],
      reasoning_effort: "off".into(),
      web_search: false,
      initial_request: false,
      client_id: Uuid::new_v4(),
    };
    assert!(validate(&p).is_ok());
    p.images.push(json!({"data":"file:///private"}));
    assert!(validate(&p).is_err());
    p.images.clear();
    p.model.is_internal = false;
    p.model.base_url = "http://localhost:11434".into();
    p.model.api_key = "key".into();
    assert!(validate(&p).is_err());
    p.model.base_url = "https://model.example/v1".into();
    assert!(validate(&p).is_ok());
    p.reasoning_effort = "unlimited".into();
    assert!(validate(&p).is_err());
  }
}
