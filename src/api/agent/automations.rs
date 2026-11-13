//! WeKnora owns the scheduler and the run loop; Cloud scopes every automation
//! to a workspace and keeps model credentials out of the client.
use super::conversations::{knowledge_account, unavailable, workspace_user};
use super::gateway;
use crate::{biz::authentication::jwt::Authorization, state::AppState};
use actix_web::{error, web, HttpResponse, Scope};
use ponynotes_weknora::Client;
use reqwest::Method;
use secrecy::{ExposeSecret, Secret};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashSet;
use uuid::Uuid;

type WorkspacePath = web::Path<Uuid>;
type AutomationPath = web::Path<(Uuid, String)>;

pub fn scope() -> Scope {
  web::scope("/automations/{workspace_id}")
    .app_data(web::JsonConfig::default().limit(1024 * 1024))
    .route("/validate-trigger", web::post().to(validate_trigger))
    .route("", web::get().to(list))
    .route("", web::post().to(create))
    .route("/runs/{run_id}/cancel", web::post().to(cancel_run))
    .route("/{automation_id}", web::get().to(detail))
    .route("/{automation_id}", web::put().to(update))
    .route("/{automation_id}", web::delete().to(remove))
    .route("/{automation_id}/pause", web::post().to(pause))
    .route("/{automation_id}/resume", web::post().to(resume))
    .route("/{automation_id}/run", web::post().to(run))
    .route("/{automation_id}/runs", web::get().to(runs))
}

struct Context {
  uid: i64,
  workspace: Uuid,
  token: String,
}

async fn context(
  auth: &Authorization,
  workspace: Uuid,
  state: &AppState,
  client: &Client,
) -> actix_web::Result<Context> {
  if !ponynotes_weknora::enabled() {
    return Err(error::ErrorServiceUnavailable("小马 Agent尚未启用"));
  }
  let uid = workspace_user(auth, workspace, state).await?;
  let account = knowledge_account(auth, state).await?;
  let scope = client
    .conversation_scope(&account, auth.uuid()?, &auth.token, workspace)
    .await
    .map_err(unavailable)?;
  Ok(Context {
    uid,
    workspace,
    token: scope.token,
  })
}

fn automation_id(id: &str) -> actix_web::Result<String> {
  Uuid::parse_str(id)
    .map(|id| id.to_string())
    .map_err(|_| error::ErrorBadRequest("自动化任务 ID 无效"))
}

/// An automation only exists for the workspace that registered it.
async fn owned(ctx: &Context, id: &str, state: &AppState) -> actix_web::Result<Option<String>> {
  sqlx::query_scalar::<_, Option<String>>(
    "SELECT model_id FROM af_agent_automations \
     WHERE automation_id = $1 AND uid = $2 AND workspace_id = $3",
  )
  .bind(id)
  .bind(ctx.uid)
  .bind(ctx.workspace)
  .fetch_optional(&state.pg_pool)
  .await
  .map_err(unavailable)?
  .ok_or_else(|| error::ErrorNotFound("自动化任务不存在"))
}

#[derive(Deserialize, Default)]
struct Model {
  #[serde(default)]
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

#[derive(Deserialize, Default)]
struct Save {
  #[serde(default)]
  name: String,
  #[serde(default)]
  description: String,
  #[serde(default)]
  instruction: String,
  #[serde(default)]
  trigger: Value,
  #[serde(default)]
  session_mode: String,
  #[serde(default)]
  carry_last_result: bool,
  #[serde(default)]
  misfire_policy: String,
  #[serde(default)]
  overlap_policy: String,
  #[serde(default)]
  approval_timeout_minutes: Option<i64>,
  #[serde(default)]
  limits: Option<Value>,
  #[serde(default)]
  notify_on_success: Option<bool>,
  #[serde(default)]
  enabled: Option<bool>,
  #[serde(default)]
  model: Option<Model>,
}

fn clean_name(name: &str) -> actix_web::Result<String> {
  let name = name.trim();
  if name.is_empty() || name.chars().count() > 100 || name.chars().any(char::is_control) {
    return Err(error::ErrorBadRequest("自动化名称需要 1–100 个字符"));
  }
  Ok(name.to_owned())
}

fn trigger_ok(trigger: &Value) -> bool {
  if let Some(timezone) = trigger["timezone"].as_str() {
    if timezone.len() > 64 || timezone.chars().any(char::is_control) {
      return false;
    }
  }
  match trigger["type"].as_str() {
    Some("cron") => trigger["expr"]
      .as_str()
      .is_some_and(|expr| !expr.trim().is_empty() && expr.len() <= 100),
    Some("once") => trigger["at"]
      .as_str()
      .is_some_and(|at| !at.trim().is_empty() && at.len() <= 40),
    _ => false,
  }
}

fn check_model(config: &Model) -> actix_web::Result<()> {
  if config.model_name.trim().is_empty() || config.model_name.chars().count() > 200 {
    return Err(error::ErrorBadRequest("请选择模型"));
  }
  if config.is_internal {
    return Ok(());
  }
  let url = reqwest::Url::parse(&config.base_url)
    .map_err(|_| error::ErrorBadRequest("自定义模型地址无效"))?;
  if url.scheme() != "https"
    || url.host_str().is_none()
    || !url.username().is_empty()
    || url.password().is_some()
    || url.query().is_some()
    || url.fragment().is_some()
    || config.api_key.trim().is_empty()
  {
    return Err(error::ErrorBadRequest(
      "小马 Agent的自定义模型需要服务端可访问的 HTTPS 地址和 API Key",
    ));
  }
  Ok(())
}

fn editable(payload: &Save) -> actix_web::Result<Value> {
  let mut body = json!({
    "name": clean_name(&payload.name)?,
    "description": payload.description.trim().chars().take(500).collect::<String>(),
    "instruction": payload.instruction.clone(),
    "trigger": payload.trigger.clone(),
    "session_mode": payload.session_mode.clone(),
    "carry_last_result": payload.carry_last_result,
    "misfire_policy": payload.misfire_policy.clone(),
    "overlap_policy": payload.overlap_policy.clone(),
  });
  if payload.instruction.trim().is_empty() || payload.instruction.chars().count() > 20000 {
    return Err(error::ErrorBadRequest("自动化指令需要 1–20000 个字符"));
  }
  if !matches!(payload.session_mode.as_str(), "per_run" | "persistent") {
    return Err(error::ErrorBadRequest("会话模式无效"));
  }
  if !matches!(payload.misfire_policy.as_str(), "skip" | "run_once") {
    return Err(error::ErrorBadRequest("错过策略无效"));
  }
  if !matches!(payload.overlap_policy.as_str(), "skip" | "queue") {
    return Err(error::ErrorBadRequest("重叠策略无效"));
  }
  if !trigger_ok(&payload.trigger) {
    return Err(error::ErrorBadRequest("触发条件无效"));
  }
  if let Some(minutes) = payload.approval_timeout_minutes {
    if !(1..=1440).contains(&minutes) {
      return Err(error::ErrorBadRequest("审批超时时间无效"));
    }
    body["approval_timeout_minutes"] = json!(minutes);
  }
  if let Some(limits) = &payload.limits {
    body["limits"] = limits.clone();
  }
  if let Some(notify) = payload.notify_on_success {
    body["notify_on_success"] = json!(notify);
  }
  if let Some(enabled) = payload.enabled {
    body["enabled"] = json!(enabled);
  }
  Ok(body)
}

/// Automation model records live in WeKnora but always point back at Cloud, so
/// every run is metered by sub2api like an interactive conversation.
async fn configure_model(
  client: &Client,
  ctx: &Context,
  state: &AppState,
  config: &Model,
  existing: Option<&str>,
) -> actix_web::Result<String> {
  let (base, credential) = if config.is_internal {
    let token: Secret<String> = gateway(state)?.automation_token(ctx.uid)?;
    (client.model_base_url(), token.expose_secret().to_owned())
  } else {
    (
      config
        .base_url
        .trim_end_matches('/')
        .trim_end_matches("/chat/completions")
        .to_owned(),
      config.api_key.clone(),
    )
  };
  let payload = json!({
    "name": config.model_name,
    "display_name": "小马笔记 · 自动化",
    "type": "KnowledgeQA",
    "source": "remote",
    "parameters": {
      "base_url": base,
      "api_key": credential,
      "provider": "generic",
      "interface_type": "openai",
      "supports_vision": true,
      "context_window": 128000,
      "max_output_tokens": 8192,
      "spec": {
        "reasoning": config.supports_reasoning,
        "context_window": 128000,
        "max_output_tokens": 8192
      }
    }
  });
  if let Some(id) = existing {
    client
      .agent_request(
        &ctx.token,
        &format!("models/{id}"),
        Method::PUT,
        Some(&payload),
      )
      .await
      .map_err(unavailable)?;
    client
      .agent_request(
        &ctx.token,
        &format!("models/{id}/credentials"),
        Method::PUT,
        Some(&json!({"api_key": credential})),
      )
      .await
      .map_err(unavailable)?;
    return Ok(id.to_owned());
  }
  let created = client
    .agent_request(&ctx.token, "models", Method::POST, Some(&payload))
    .await
    .map_err(unavailable)?;
  created["data"]["id"]
    .as_str()
    .map(str::to_owned)
    .ok_or_else(|| unavailable("missing model"))
}

async fn list(
  auth: Authorization,
  path: WorkspacePath,
  state: web::Data<AppState>,
  client: web::Data<Client>,
) -> actix_web::Result<HttpResponse> {
  let ctx = context(&auth, *path, &state, &client).await?;
  let managed: HashSet<String> = sqlx::query_scalar::<_, String>(
    "SELECT automation_id FROM af_agent_automations WHERE uid = $1 AND workspace_id = $2",
  )
  .bind(ctx.uid)
  .bind(ctx.workspace)
  .fetch_all(&state.pg_pool)
  .await
  .map_err(unavailable)?
  .into_iter()
  .collect();
  let upstream = client
    .agent_request(&ctx.token, "automations", Method::GET, None)
    .await
    .map_err(unavailable)?;
  let automations: Vec<Value> = upstream["data"]
    .as_array()
    .map(|items| {
      items
        .iter()
        .filter(|item| item["id"].as_str().is_some_and(|id| managed.contains(id)))
        .cloned()
        .collect()
    })
    .unwrap_or_default();
  Ok(
    HttpResponse::Ok()
      .insert_header(("Cache-Control", "no-store"))
      .json(json!({"automations": automations})),
  )
}

async fn create(
  auth: Authorization,
  path: WorkspacePath,
  state: web::Data<AppState>,
  client: web::Data<Client>,
  payload: web::Json<Save>,
) -> actix_web::Result<HttpResponse> {
  let ctx = context(&auth, *path, &state, &client).await?;
  let model = payload
    .model
    .as_ref()
    .ok_or_else(|| error::ErrorBadRequest("请选择模型"))?;
  check_model(model)?;
  let mut body = editable(&payload)?;
  let model_id = configure_model(&client, &ctx, &state, model, None).await?;
  body["model_id"] = json!(model_id);
  let created = client
    .agent_request(&ctx.token, "automations", Method::POST, Some(&body))
    .await
    .map_err(unavailable)?;
  let id = created["data"]["id"]
    .as_str()
    .ok_or_else(|| unavailable("missing automation"))?
    .to_owned();
  sqlx::query(
    "INSERT INTO af_agent_automations (automation_id, uid, workspace_id, name, model_id) \
     VALUES ($1, $2, $3, $4, $5)",
  )
  .bind(&id)
  .bind(ctx.uid)
  .bind(ctx.workspace)
  .bind(body["name"].as_str().unwrap_or_default())
  .bind(&model_id)
  .execute(&state.pg_pool)
  .await
  .map_err(unavailable)?;
  Ok(
    HttpResponse::Ok()
      .insert_header(("Cache-Control", "no-store"))
      .json(json!({"automation": created["data"]})),
  )
}

async fn detail(
  auth: Authorization,
  path: AutomationPath,
  state: web::Data<AppState>,
  client: web::Data<Client>,
) -> actix_web::Result<HttpResponse> {
  let (workspace, raw) = &*path;
  let ctx = context(&auth, *workspace, &state, &client).await?;
  let id = automation_id(raw)?;
  owned(&ctx, &id, &state).await?;
  let upstream = client
    .agent_request(&ctx.token, &format!("automations/{id}"), Method::GET, None)
    .await
    .map_err(unavailable)?;
  Ok(
    HttpResponse::Ok()
      .insert_header(("Cache-Control", "no-store"))
      .json(json!({"automation": upstream["data"]})),
  )
}

async fn update(
  auth: Authorization,
  path: AutomationPath,
  state: web::Data<AppState>,
  client: web::Data<Client>,
  payload: web::Json<Save>,
) -> actix_web::Result<HttpResponse> {
  let (workspace, raw) = &*path;
  let ctx = context(&auth, *workspace, &state, &client).await?;
  let id = automation_id(raw)?;
  let existing = owned(&ctx, &id, &state).await?;
  let mut body = editable(&payload)?;
  // Re-sending the whole record lets a bare pause/enable toggle avoid repeating
  // model credentials the client never sees.
  let model_id = match payload.model.as_ref() {
    Some(model) => {
      check_model(model)?;
      configure_model(&client, &ctx, &state, model, existing.as_deref()).await?
    },
    None => existing
      .clone()
      .ok_or_else(|| error::ErrorBadRequest("请重新选择模型后再保存自动化任务"))?,
  };
  body["model_id"] = json!(model_id);
  let updated = client
    .agent_request(
      &ctx.token,
      &format!("automations/{id}"),
      Method::PUT,
      Some(&body),
    )
    .await
    .map_err(unavailable)?;
  sqlx::query(
    "UPDATE af_agent_automations SET name = $4, model_id = $5, updated_at = now() \
     WHERE automation_id = $1 AND uid = $2 AND workspace_id = $3",
  )
  .bind(&id)
  .bind(ctx.uid)
  .bind(ctx.workspace)
  .bind(body["name"].as_str().unwrap_or_default())
  .bind(&model_id)
  .execute(&state.pg_pool)
  .await
  .map_err(unavailable)?;
  Ok(
    HttpResponse::Ok()
      .insert_header(("Cache-Control", "no-store"))
      .json(json!({"automation": updated["data"]})),
  )
}

async fn remove(
  auth: Authorization,
  path: AutomationPath,
  state: web::Data<AppState>,
  client: web::Data<Client>,
) -> actix_web::Result<HttpResponse> {
  let (workspace, raw) = &*path;
  let ctx = context(&auth, *workspace, &state, &client).await?;
  let id = automation_id(raw)?;
  owned(&ctx, &id, &state).await?;
  client
    .agent_request(
      &ctx.token,
      &format!("automations/{id}"),
      Method::DELETE,
      None,
    )
    .await
    .map_err(unavailable)?;
  sqlx::query(
    "DELETE FROM af_agent_automations \
     WHERE automation_id = $1 AND uid = $2 AND workspace_id = $3",
  )
  .bind(&id)
  .bind(ctx.uid)
  .bind(ctx.workspace)
  .execute(&state.pg_pool)
  .await
  .map_err(unavailable)?;
  Ok(HttpResponse::NoContent().finish())
}

async fn control(
  action: &str,
  ctx: &Context,
  client: &Client,
  id: &str,
) -> actix_web::Result<Value> {
  client
    .agent_request(
      &ctx.token,
      &format!("automations/{id}/{action}"),
      Method::POST,
      None,
    )
    .await
    .map_err(unavailable)
}

macro_rules! control_handler {
  ($name:ident, $action:literal) => {
    async fn $name(
      auth: Authorization,
      path: AutomationPath,
      state: web::Data<AppState>,
      client: web::Data<Client>,
    ) -> actix_web::Result<HttpResponse> {
      let (workspace, raw) = &*path;
      let ctx = context(&auth, *workspace, &state, &client).await?;
      let id = automation_id(raw)?;
      owned(&ctx, &id, &state).await?;
      let result = control($action, &ctx, &client, &id).await?;
      Ok(
        HttpResponse::Ok()
          .insert_header(("Cache-Control", "no-store"))
          .json(result),
      )
    }
  };
}

control_handler!(pause, "pause");
control_handler!(resume, "resume");
control_handler!(run, "run");

#[derive(Deserialize, Default)]
struct Page {
  #[serde(default)]
  page: Option<u32>,
  #[serde(default)]
  page_size: Option<u32>,
}

async fn runs(
  auth: Authorization,
  path: AutomationPath,
  state: web::Data<AppState>,
  client: web::Data<Client>,
  query: web::Query<Page>,
) -> actix_web::Result<HttpResponse> {
  let (workspace, raw) = &*path;
  let ctx = context(&auth, *workspace, &state, &client).await?;
  let id = automation_id(raw)?;
  owned(&ctx, &id, &state).await?;
  let page = query.page.unwrap_or(1).max(1);
  let page_size = query.page_size.unwrap_or(20).clamp(1, 100);
  let upstream = client
    .agent_request(
      &ctx.token,
      &format!("automations/{id}/runs?page={page}&page_size={page_size}"),
      Method::GET,
      None,
    )
    .await
    .map_err(unavailable)?;
  Ok(
    HttpResponse::Ok()
      .insert_header(("Cache-Control", "no-store"))
      .json(json!({
        "runs": upstream["data"],
        "total": upstream["total"],
        "page": page,
        "page_size": page_size,
      })),
  )
}

async fn cancel_run(
  auth: Authorization,
  path: AutomationPath,
  state: web::Data<AppState>,
  client: web::Data<Client>,
) -> actix_web::Result<HttpResponse> {
  let (workspace, raw) = &*path;
  let ctx = context(&auth, *workspace, &state, &client).await?;
  let run_id = automation_id(raw)?;
  let upstream = client
    .agent_request(
      &ctx.token,
      &format!("automation-runs/{run_id}/cancel"),
      Method::POST,
      None,
    )
    .await
    .map_err(unavailable)?;
  Ok(
    HttpResponse::Ok()
      .insert_header(("Cache-Control", "no-store"))
      .json(upstream),
  )
}

#[derive(Deserialize, Default)]
struct Trigger {
  #[serde(default)]
  trigger: Value,
}

async fn validate_trigger(
  auth: Authorization,
  path: WorkspacePath,
  state: web::Data<AppState>,
  client: web::Data<Client>,
  payload: web::Json<Trigger>,
) -> actix_web::Result<HttpResponse> {
  let ctx = context(&auth, *path, &state, &client).await?;
  if !trigger_ok(&payload.trigger) {
    return Err(error::ErrorBadRequest("触发条件无效"));
  }
  let upstream = client
    .agent_request(
      &ctx.token,
      "automations/validate-trigger",
      Method::POST,
      Some(&json!({"trigger": payload.trigger})),
    )
    .await
    .map_err(unavailable)?;
  Ok(
    HttpResponse::Ok()
      .insert_header(("Cache-Control", "no-store"))
      .json(json!({"next_runs": upstream["data"]["next_runs"]})),
  )
}
