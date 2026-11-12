//! Cloud owns identity, note scope and billing; WeKnora owns the agent loop.
use super::{gateway, issue_session_token};
use crate::{biz::authentication::jwt::Authorization, state::AppState};
use access_control::act::Action;
use actix_web::{error, web, FromRequest, HttpRequest, HttpResponse, Scope};
use appflowy_collaborate::ws2::WorkspaceCollabInstanceCache;
use base64::Engine;
use futures_util::{stream, StreamExt};
use ponynotes_weknora::references::{restore_reference_input, Catalog, Reference};
use ponynotes_weknora::{agent, Client};
use redis::AsyncCommands;
use reqwest::Method;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;
use std::collections::HashSet;
use uuid::Uuid;

pub(super) type Document = web::Path<(Uuid, Uuid)>;

pub fn collection_scope() -> Scope {
  web::scope("/conversations/{workspace_id}")
    .route("", web::get().to(list_conversations))
    .route("", web::post().to(create_conversation))
}

async fn knowledge_account(auth: &Authorization, state: &AppState) -> actix_web::Result<Value> {
  let account: Option<Value> =
    sqlx::query_scalar("SELECT state->'users'->$1 FROM af_weknora_sync_state WHERE singleton")
      .bind(auth.uuid()?.to_string())
      .fetch_optional(&state.pg_pool)
      .await
      .map_err(unavailable)?
      .flatten();
  account.ok_or_else(|| error::ErrorServiceUnavailable("知识库账户尚未同步，请稍后重试"))
}

pub(super) async fn reference_catalog(
  auth: Authorization,
  workspace: web::Path<Uuid>,
  state: web::Data<AppState>,
  client: web::Data<Client>,
) -> actix_web::Result<HttpResponse> {
  workspace_user(&auth, *workspace, &state).await?;
  if !ponynotes_weknora::enabled() {
    return Err(error::ErrorServiceUnavailable("知识库尚未启用"));
  }
  let account = knowledge_account(&auth, &state).await?;
  let scope = client
    .conversation_scope(&account, auth.uuid()?, &auth.token, *workspace)
    .await
    .map_err(unavailable)?;
  let catalog = client
    .reference_catalog(&account, *workspace, &scope)
    .await
    .map_err(unavailable)?;
  Ok(
    HttpResponse::Ok()
      .insert_header(("Cache-Control", "no-store"))
      .json(json!({"items":catalog.items})),
  )
}

#[derive(Deserialize)]
pub(super) struct ReferencePage {
  #[serde(default = "first_page")]
  page: u32,
}
fn first_page() -> u32 {
  1
}

pub(super) async fn reference_file(
  auth: Authorization,
  path: web::Path<(Uuid, Uuid)>,
  page: web::Query<ReferencePage>,
  state: web::Data<AppState>,
  client: web::Data<Client>,
) -> actix_web::Result<HttpResponse> {
  if !ponynotes_weknora::enabled() {
    return Err(error::ErrorServiceUnavailable("知识库尚未启用"));
  }
  let (workspace, id) = *path;
  workspace_user(&auth, workspace, &state).await?;
  if page.page == 0 || page.page > 100_000 {
    return Err(error::ErrorBadRequest("页码无效"));
  }
  let account = knowledge_account(&auth, &state).await?;
  let scope = client
    .conversation_scope(&account, auth.uuid()?, &auth.token, workspace)
    .await
    .map_err(unavailable)?;
  let catalog = client
    .reference_catalog(&account, workspace, &scope)
    .await
    .map_err(unavailable)?;
  let source = catalog
    .sources
    .get(&id.to_string())
    .filter(|s| s["base_id"].is_string())
    .ok_or_else(|| error::ErrorForbidden("文件已删除或无权访问"))?;
  let chunks = client
    .agent_request(
      &scope.token,
      &format!("chunks/{id}?page={}&page_size=25", page.page),
      Method::GET,
      None,
    )
    .await
    .map_err(unavailable)?;
  Ok(
    HttpResponse::Ok()
      .insert_header(("Cache-Control", "no-store"))
      .json(json!({"title":source["title"],"chunks":chunks["data"],"total":chunks["total"]})),
  )
}

async fn workspace_user(
  auth: &Authorization,
  workspace: Uuid,
  state: &AppState,
) -> actix_web::Result<i64> {
  let uid = state.user_cache.get_user_uid(&auth.uuid()?).await?;
  state
    .workspace_access_control
    .enforce_action(&uid, &workspace, Action::Read)
    .await
    .map_err(|_| error::ErrorForbidden("无权访问此工作区"))?;
  Ok(uid)
}

#[derive(Deserialize, Default)]
struct ListConversations {
  #[serde(default)]
  offset: u32,
}

async fn list_conversations(
  auth: Authorization,
  workspace: web::Path<Uuid>,
  state: web::Data<AppState>,
  query: web::Query<ListConversations>,
) -> actix_web::Result<HttpResponse> {
  let uid = workspace_user(&auth, *workspace, &state).await?;
  let rows: Vec<Value> = sqlx::query_scalar("SELECT jsonb_build_object('id',conversation_id,'title',title,'created_at',created_at,'updated_at',updated_at) FROM af_agent_conversations WHERE uid=$1 AND workspace_id=$2 ORDER BY updated_at DESC,conversation_id LIMIT 100 OFFSET $3")
    .bind(uid).bind(*workspace).bind(i64::from(query.offset)).fetch_all(&state.pg_pool).await.map_err(unavailable)?;
  Ok(
    HttpResponse::Ok()
      .insert_header(("Cache-Control", "no-store"))
      .json(json!({"conversations":rows})),
  )
}

#[derive(Deserialize)]
struct CreateConversation {
  id: Uuid,
  title: String,
}

fn clean_title(title: &str) -> actix_web::Result<String> {
  let title = title.trim();
  if title.is_empty() || title.chars().count() > 100 || title.chars().any(char::is_control) {
    return Err(error::ErrorBadRequest("对话标题无效"));
  }
  Ok(title.to_owned())
}

async fn create_conversation(
  auth: Authorization,
  workspace: web::Path<Uuid>,
  state: web::Data<AppState>,
  body: web::Json<CreateConversation>,
) -> actix_web::Result<HttpResponse> {
  let uid = workspace_user(&auth, *workspace, &state).await?;
  let title = clean_title(&body.title)?;
  sqlx::query("INSERT INTO af_agent_conversations(conversation_id,uid,workspace_id,title) VALUES($1,$2,$3,$4) ON CONFLICT DO NOTHING")
    .bind(body.id).bind(uid).bind(*workspace).bind(title).execute(&state.pg_pool).await.map_err(unavailable)?;
  let result: Option<Value> = sqlx::query_scalar("SELECT jsonb_build_object('id',conversation_id,'title',title,'created_at',created_at,'updated_at',updated_at) FROM af_agent_conversations WHERE conversation_id=$1 AND uid=$2 AND workspace_id=$3")
    .bind(body.id).bind(uid).bind(*workspace).fetch_optional(&state.pg_pool).await.map_err(unavailable)?;
  Ok(
    HttpResponse::Ok()
      .insert_header(("Cache-Control", "no-store"))
      .json(result.ok_or_else(|| error::ErrorConflict("对话标识已使用，请重试"))?),
  )
}

async fn independent_owner(
  state: &AppState,
  workspace: Uuid,
  conversation: Uuid,
  uid: i64,
) -> actix_web::Result<bool> {
  let owner: Option<i64> = sqlx::query_scalar(
    "SELECT uid FROM af_agent_conversations WHERE workspace_id=$1 AND conversation_id=$2",
  )
  .bind(workspace)
  .bind(conversation)
  .fetch_optional(&state.pg_pool)
  .await
  .map_err(unavailable)?;
  match owner {
    Some(owner) if owner != uid => Err(error::ErrorForbidden("无权访问此 AI 对话")),
    Some(_) => Ok(true),
    None => Ok(false),
  }
}

fn artifact_document(file: &str) -> actix_web::Result<Option<Uuid>> {
  let Some(rest) = file.strip_prefix("pn-agent-v1-") else {
    return Ok(None);
  };
  let id = rest
    .get(..36)
    .filter(|_| rest.as_bytes().get(36) == Some(&b'-'))
    .ok_or_else(|| error::ErrorBadRequest("无效的产物标识"))?;
  Uuid::parse_str(id)
    .map(Some)
    .map_err(|_| error::ErrorBadRequest("无效的产物文档"))
}

/// Only the new artifact namespace requires document access; existing blob callers keep their contract.
pub(crate) async fn authorize_artifact_blob(
  state: &AppState,
  req: &HttpRequest,
  workspace: Uuid,
  file: &str,
) -> actix_web::Result<bool> {
  let Some(document) = artifact_document(file)? else {
    return Ok(false);
  };
  let auth = Authorization::extract(req).await?;
  let uid = state.user_cache.get_user_uid(&auth.uuid()?).await?;
  state
    .workspace_access_control
    .enforce_action(&uid, &workspace, Action::Read)
    .await
    .map_err(|_| error::ErrorForbidden("无权访问产物所在工作区"))?;
  if independent_owner(state, workspace, document, uid).await? {
    return Ok(true);
  }
  let folder = state.ws_server.get_folder(workspace).await?;
  let visibility = crate::biz::collab::folder_view::private_space_and_trash_view_ids(uid, &folder)?;
  let mut current = document;
  let mut visited = HashSet::new();
  while current != workspace {
    if !visited.insert(current)
      || visibility.other_private_space_ids.contains(&current)
      || visibility.view_ids_in_trash.contains(&current)
    {
      return Err(error::ErrorForbidden("无权访问产物所属文档"));
    }
    let view = folder
      .get_view(&current.to_string(), uid)
      .ok_or_else(|| error::ErrorForbidden("产物所属文档不存在或不可访问"))?;
    current = Uuid::parse_str(&view.parent_view_id)
      .map_err(|_| error::ErrorForbidden("产物所属文档不在当前工作区"))?;
  }
  Ok(true)
}

pub fn scope() -> Scope {
  web::scope("/conversations/{workspace_id}/{document_id}")
    .app_data(web::JsonConfig::default().limit(30 * 1024 * 1024))
    .route("", web::get().to(history))
    .route("/messages", web::post().to(send))
    .route("/stop", web::post().to(stop))
    .route("/steer", web::post().to(steer))
    .route("/stream", web::get().to(resume))
    .route("/artifacts", web::post().to(save_cloud_artifact))
    .route(
      "/messages/{message_id}/artifacts/{index}",
      web::get().to(download_artifact),
    )
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
  independent: bool,
  account: Value,
  catalog: Option<Catalog>,
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
  let account = knowledge_account(auth, state).await?;
  let independent = independent_owner(state, workspace, document, uid).await?;
  let mut scope = if independent {
    client
      .conversation_scope(&account, uuid, &auth.token, workspace)
      .await
  } else {
    client
      .agent_scope(&account, uuid, &auth.token, workspace, document)
      .await
  }
  .map_err(unavailable)?;
  let retained: Option<Value> = sqlx::query_scalar("SELECT state->'reference_items' FROM af_weknora_agent_sessions WHERE uid=$1 AND workspace_id=$2 AND document_id=$3")
    .bind(uid).bind(workspace).bind(document).fetch_optional(&state.pg_pool).await.map_err(unavailable)?.flatten();
  let retained: Vec<Reference> =
    serde_json::from_value(retained.unwrap_or(json!([]))).map_err(unavailable)?;
  let catalog = if retained.is_empty() {
    None
  } else {
    let catalog = client
      .reference_catalog(&account, workspace, &scope)
      .await
      .map_err(unavailable)?;
    scope
      .documents
      .extend(catalog.resolve(&retained, false).map_err(unavailable)?);
    Some(catalog)
  };
  Ok(Context {
    uid,
    workspace,
    document,
    scope,
    independent,
    account,
    catalog,
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
  let artifacts: Value = if ctx.independent {
    sqlx::query_scalar("SELECT artifacts FROM af_agent_conversations WHERE uid=$1 AND workspace_id=$2 AND conversation_id=$3")
      .bind(ctx.uid).bind(ctx.workspace).bind(ctx.document).fetch_one(&state.pg_pool).await.map_err(unavailable)?
  } else {
    json!({})
  };
  let changed = record["session_id"].is_string() && !scope_matches(&ctx, &record);
  let mut messages = if !changed {
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
  for message in &mut messages {
    restore_reference_input(message, &record);
    if let Some(files) = message.get_mut("artifacts").and_then(Value::as_array_mut) {
      *files = public_artifacts(files);
    }
  }
  Ok(
    HttpResponse::Ok()
      .insert_header(("Cache-Control", "no-store"))
      .json(json!({
    "messages":messages,"sources":ctx.scope.documents,"scope_changed":changed,"mode":record["mode"],"cloud_artifacts":artifacts,
    "active_message_id":active_message(&messages).and_then(|m|m["id"].as_str())})),
  )
}

#[derive(Deserialize)]
struct CloudArtifact {
  message_id: Uuid,
  index: u32,
  file_name: String,
  file_size: u64,
  cloud_url: String,
}

async fn save_cloud_artifact(
  auth: Authorization,
  path: Document,
  state: web::Data<AppState>,
  body: web::Json<CloudArtifact>,
) -> actix_web::Result<HttpResponse> {
  let (workspace, conversation) = *path;
  let uid = workspace_user(&auth, workspace, &state).await?;
  if !independent_owner(&state, workspace, conversation, uid).await? {
    return Err(error::ErrorNotFound("独立会话不存在"));
  }
  let url =
    reqwest::Url::parse(&body.cloud_url).map_err(|_| error::ErrorBadRequest("产物地址无效"))?;
  let prefix = format!(
    "/api/file_storage/{workspace}/blob/pn-agent-v1-{conversation}-{}-{}",
    body.message_id, body.index
  );
  if body.file_name.is_empty()
    || body.file_name.len() > 255
    || body.file_size > 3 * 1024 * 1024 * 1024
    || !url.path().starts_with(&prefix)
    || !url.username().is_empty()
    || url.password().is_some()
    || url.query().is_some()
  {
    return Err(error::ErrorBadRequest("产物元数据无效"));
  }
  let key = format!("{}/{}", body.message_id, body.index);
  let file = json!({"message_id":body.message_id,"index":body.index,"file_name":body.file_name,"file_size":body.file_size,"cloud_url":body.cloud_url});
  sqlx::query("UPDATE af_agent_conversations SET artifacts=jsonb_set(artifacts,ARRAY[$4]::text[],$5,true) WHERE uid=$1 AND workspace_id=$2 AND conversation_id=$3")
    .bind(uid).bind(workspace).bind(conversation).bind(key).bind(file).execute(&state.pg_pool).await.map_err(unavailable)?;
  Ok(HttpResponse::NoContent().finish())
}

fn public_artifacts(files: &[Value]) -> Vec<Value> {
  files.iter().enumerate().filter(|(_, file)| file["deleted_at"].is_null()).map(|(index, file)| {
    json!({"index":index,"file_name":file["file_name"],"file_size":file["file_size"],"file_type":file["file_type"],"handle":file["handle"]})
  }).collect()
}

async fn download_artifact(
  auth: Authorization,
  path: web::Path<(Uuid, Uuid, Uuid, u32)>,
  state: web::Data<AppState>,
  client: web::Data<Client>,
) -> actix_web::Result<HttpResponse> {
  let (workspace, document, message, index) = *path;
  let document = web::Path::from((workspace, document));
  let ctx = context(&auth, &document, &state, &client).await?;
  let record = stored(&ctx, &state).await?;
  if !scope_matches(&ctx, &record) {
    return Err(error::ErrorForbidden("笔记授权范围已变化"));
  }
  let upstream = client
    .agent_file(
      &ctx.scope.token,
      &format!(
        "sessions/{}/messages/{message}/artifacts/{index}/download",
        session_id(&record)?
      ),
    )
    .await
    .map_err(unavailable)?;
  Ok(
    HttpResponse::Ok()
      .content_type("application/octet-stream")
      .insert_header(("Cache-Control", "no-store"))
      .streaming(
        upstream
          .bytes_stream()
          .map(|chunk| chunk.map_err(unavailable)),
      ),
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
  #[serde(default = "default_mode")]
  mode: String,
  #[serde(default)]
  attachments: Vec<Value>,
  #[serde(default)]
  references: Vec<Reference>,
}

fn default_mode() -> String {
  "smart-reasoning".into()
}

fn validate(payload: &Send) -> actix_web::Result<()> {
  if payload.query.trim().is_empty()
    || payload.query.chars().count() > 100_000
    || payload.model.model_name.trim().is_empty()
    || payload.model.model_name.len() > 256
    || payload.images.len() + payload.attachments.len() > 4
    || payload.references.len() > 20
    || payload.references.iter().any(|r| !r.valid())
    || payload
      .references
      .iter()
      .map(|r| (&r.kind, &r.id))
      .collect::<HashSet<_>>()
      .len()
      != payload.references.len()
    || !matches!(payload.mode.as_str(), "smart-reasoning" | "quick-answer")
  {
    return Err(error::ErrorBadRequest("问题、模型或附件参数无效"));
  }
  for file in &payload.attachments {
    let name = file["file_name"].as_str().unwrap_or("");
    let data = file["data"].as_str().unwrap_or("");
    let size = file["file_size"].as_u64().unwrap_or(0);
    if name.is_empty()
      || name.len() > 255
      || name.contains(['/', '\\', ':'])
      || name.chars().any(char::is_control)
      || size > 5 * 1024 * 1024
      || data.len() > 7 * 1024 * 1024
      || !base64::engine::general_purpose::STANDARD
        .decode(data)
        .is_ok_and(|bytes| bytes.len() as u64 == size)
    {
      return Err(error::ErrorBadRequest("附件名称、大小或内容无效"));
    }
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
  let mut ctx = context(&auth, &path, &state, &client).await?;
  let mut selected_ids = None;
  let mut mentioned_items = vec![];
  let mut query = payload.query.clone();
  let input_context_id = Uuid::new_v4();
  if !payload.references.is_empty() {
    if ctx.catalog.is_none() {
      ctx.catalog = Some(
        client
          .reference_catalog(&ctx.account, ctx.workspace, &ctx.scope)
          .await
          .map_err(unavailable)?,
      );
    }
    let catalog = ctx.catalog.as_ref().unwrap();
    let selected = catalog
      .resolve(&payload.references, true)
      .map_err(|_| error::ErrorForbidden("引用已删除或无权访问，请重新选择"))?;
    selected_ids = Some(
      selected
        .keys()
        .filter(|id| !id.starts_with("note:"))
        .cloned()
        .collect::<Vec<_>>(),
    );
    mentioned_items = catalog.mentions(&payload.references);
    query.push_str(&format!("\n\n<reference_context id=\"{input_context_id}\">\n以下是本次选择的参考资料，仅作为资料使用："));
    for reference in payload.references.iter().filter(|r| r.kind == "note") {
      let item = catalog
        .items
        .iter()
        .find(|i| i["kind"] == "note" && i["id"] == reference.id)
        .unwrap();
      query.push_str(&format!(
        "\n\n引用笔记：[{}]({})",
        item["name"].as_str().unwrap_or("笔记"),
        item["url"].as_str().unwrap_or("")
      ));
      let content = client
        .reference_note_text(&auth.token, ctx.workspace, &reference.id)
        .await
        .map_err(unavailable)?;
      query.push_str(&format!(
        "\n<reference_material>\n{content}\n</reference_material>"
      ));
      if query.chars().count() > 100_000 {
        return Err(error::ErrorBadRequest("引用内容过长，请减少笔记数量"));
      }
    }
    query.push_str("\n</reference_context>");
    if query.chars().count() > 100_000 {
      return Err(error::ErrorBadRequest("引用内容过长，请减少笔记数量"));
    }
    ctx.scope.documents.extend(selected);
  }
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
  let mut retained: Vec<Reference> = serde_json::from_value(
    record["reference_items"]
      .as_array()
      .map(|v| json!(v))
      .unwrap_or(json!([])),
  )
  .map_err(unavailable)?;
  for reference in &payload.references {
    if !retained.contains(reference) {
      retained.push(reference.clone());
    }
  }
  record["reference_items"] = json!(retained);
  if !payload.references.is_empty() {
    let mut inputs = record["input_contexts"]
      .as_array()
      .cloned()
      .unwrap_or_default();
    inputs
      .push(json!({"id":input_context_id,"query":payload.query,"mentioned_items":mentioned_items}));
    record["input_contexts"] = json!(inputs);
  }
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
  let mut config = client
    .agent_config(
      &ctx.scope.token,
      &payload.mode,
      &model_id,
      selected_ids.as_ref().map_or(
        ctx
          .scope
          .documents
          .keys()
          .any(|id| !id.starts_with("note:")),
        |ids| !ids.is_empty(),
      ),
      payload.web_search,
    )
    .await
    .map_err(unavailable)?;
  if payload.mode == "smart-reasoning" {
    config["mcp_selection_mode"] = json!("selected");
    config["mcp_services"] = json!([mcp_id]);
  }
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
  record["mode"] = json!(payload.mode);
  save(&ctx, &record, &mut connection).await?;
  let knowledge_ids = selected_ids.unwrap_or_else(|| {
    ctx
      .scope
      .documents
      .keys()
      .filter(|id| !id.starts_with("note:"))
      .cloned()
      .collect()
  });
  // Expand a selected base to authorized document IDs; raw bases could expose hidden note copies.
  let request = json!({"query":query,"mentioned_items":mentioned_items,"agent_id":agent_id,"agent_enabled":payload.mode == "smart-reasoning","knowledge_ids":knowledge_ids,
    "knowledge_base_ids":[],"images":payload.images,"attachment_uploads":payload.attachments,"reasoning_effort":if payload.model.supports_reasoning {payload.reasoning_effort.as_str()}else{"off"},"web_search_enabled":payload.web_search,"channel":"api"});
  let upstream = client
    .agent_stream(
      &ctx.scope.token,
      &format!("agent-chat/{session}"),
      Method::POST,
      Some(&request),
    )
    .await
    .map_err(unavailable)?;
  if ctx.independent {
    let title: String = payload
      .query
      .split_whitespace()
      .collect::<Vec<_>>()
      .join(" ")
      .chars()
      .take(40)
      .collect();
    sqlx::query("UPDATE af_agent_conversations SET updated_at=now(),title=CASE WHEN title='新对话' THEN $4 ELSE title END WHERE uid=$1 AND workspace_id=$2 AND conversation_id=$3")
      .bind(ctx.uid).bind(ctx.workspace).bind(ctx.document).bind(title).execute(&mut *connection).await.map_err(unavailable)?;
  }
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
  fn independent_conversation_titles_are_bounded_and_nonempty() {
    assert_eq!(clean_title("  新对话  ").unwrap(), "新对话");
    assert!(clean_title(" ").is_err());
    assert!(clean_title("a\nprivate").is_err());
    assert!(clean_title(&"文".repeat(101)).is_err());
  }
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
      mode: default_mode(),
      attachments: vec![],
      references: vec![],
    };
    assert!(validate(&p).is_ok());
    p.references.push(Reference {
      kind: "file".into(),
      id: "../private".into(),
    });
    assert!(validate(&p).is_err());
    p.references.clear();
    p.references = vec![
      Reference {
        kind: "note".into(),
        id: Uuid::new_v4().to_string()
      };
      2
    ];
    assert!(validate(&p).is_err());
    p.references.clear();
    p.references = vec![
      Reference {
        kind: "note".into(),
        id: Uuid::new_v4().to_string()
      };
      21
    ];
    assert!(validate(&p).is_err());
    p.references.clear();
    p.attachments
      .push(json!({"file_name":"data.csv","file_size":1,"data":"YQ=="}));
    assert!(validate(&p).is_ok());
    p.attachments[0]["file_name"] = json!("../secret");
    assert!(validate(&p).is_err());
    p.attachments.clear();
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

  #[test]
  fn artifact_metadata_keeps_indices_without_storage_credentials() {
    let files = public_artifacts(&[
      json!({"deleted_at":"now"}),
      json!({"file_name":"report.pdf","url":"private://secret","content_hash":"internal","file_size":42}),
    ]);
    assert_eq!(files.len(), 1);
    assert_eq!(files[0]["index"], 1);
    assert!(files[0].get("url").is_none());
    assert!(files[0].get("content_hash").is_none());
  }

  #[test]
  fn only_artifact_blob_names_require_document_authentication() {
    let doc = Uuid::new_v4();
    assert_eq!(
      artifact_document(&format!("pn-agent-v1-{doc}-reply-0.pdf")).unwrap(),
      Some(doc)
    );
    assert_eq!(artifact_document("existing-file.pdf").unwrap(), None);
    assert!(artifact_document("pn-agent-v1-../../secret").is_err());
  }
}
