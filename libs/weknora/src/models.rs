//! Workspace-scoped model management for the shared public-library tenant.
use crate::{
  array,
  library::{require, uuid, Session},
  sync, text, Client,
};
use anyhow::{anyhow, Result};
use reqwest::Method;
use serde_json::{json, Value};
use sqlx::PgPool;

const TYPES: &[&str] = &["KnowledgeQA", "Embedding", "Rerank", "VLLM", "ASR"];
fn marker(wid: &str) -> String {
  format!("PonyNotes library model {wid}")
}
fn owned(model: &Value, wid: &str) -> bool {
  model["description"] == marker(wid)
}

pub(crate) fn visible(model: &Value, wid: &str, state: &Value) -> bool {
  let id = &model["id"];
  if !id.is_string() || !TYPES.contains(&model["type"].as_str().unwrap_or("")) {
    return false;
  }
  let description = model["description"].as_str().unwrap_or("");
  let managed = description.starts_with("PonyNotes library model ")
    || description.starts_with("PonyNotes workspace embedding ")
    || model["display_name"]
      .as_str()
      .unwrap_or("")
      .starts_with("小马笔记 · ");
  owned(model, wid)
    || model["is_builtin"] == true
    || !managed
    || *id == state["workspaces"][wid]["embedding_proxy"]["id"]
    || *id == state["service"]["summary_model_id"]
    || state["service"]["parse_models"]
      .as_array()
      .is_some_and(|ms| ms.iter().any(|m| m["id"] == *id))
}

// Allowlist the response: administrator credentials, headers and private tenant metadata never escape.
pub(crate) fn public_model(model: &Value, wid: &str) -> Value {
  let mut result = json!({});
  for field in [
    "id",
    "name",
    "display_name",
    "type",
    "source",
    "status",
    "created_at",
    "capabilities",
  ] {
    if let Some(value) = model.get(field) {
      result[field] = value.clone();
    }
  }
  result["is_builtin"] = json!(model["is_builtin"] == true);
  result["can_edit"] = json!(owned(model, wid) && model["is_builtin"] != true);
  let mut parameters = json!({});
  let fields: &[&str] = match model["type"].as_str() {
    Some("Embedding") => &["embedding_parameters"],
    Some("KnowledgeQA" | "VLLM") => &[
      "interface_type",
      "context_window",
      "max_output_tokens",
      "supports_vision",
      "spec",
    ],
    _ => &[],
  };
  for field in fields {
    if let Some(v) = model["parameters"].get(field) {
      parameters[field] = v.clone();
    }
  }
  parameters["provider"] = model["parameters"]["provider"].clone();
  if owned(model, wid) {
    parameters["base_url"] = model["parameters"]["base_url"].clone();
  }
  // Only non-secret spec facts are returned; compat can contain provider headers.
  if let Some(spec) = parameters.get_mut("spec") {
    if let Some(object) = spec.as_object_mut() {
      object.retain(|k, _| {
        [
          "api",
          "reasoning",
          "input",
          "context_window",
          "max_output_tokens",
        ]
        .contains(&k.as_str())
      });
    }
  }
  result["parameters"] = parameters;
  result
}

pub(crate) fn validate_model(input: &Value, existing: Option<&Value>, wid: &str) -> Result<Value> {
  let name = text(input, "name")?.trim();
  let kind = text(input, "type")?;
  require(
    !name.is_empty()
      && name.chars().count() <= 200
      && !name.chars().any(char::is_control)
      && TYPES.contains(&kind),
    400,
  )?;
  if let Some(previous) = existing {
    require(previous["type"] == kind, 409)?;
  }
  let source = input["source"].as_str().unwrap_or("remote");
  require(matches!(source, "remote" | "local"), 400)?;
  let params = input["parameters"]
    .as_object()
    .ok_or_else(|| anyhow!(crate::library::Rejected(400)))?;
  let common = [
    "base_url",
    "api_key",
    "provider",
    "extra_config",
    "custom_headers",
  ];
  let specific: &[&str] = match kind {
    "Embedding" => &["embedding_parameters"],
    "KnowledgeQA" | "VLLM" => &[
      "interface_type",
      "context_window",
      "max_output_tokens",
      "supports_vision",
      "spec",
    ],
    _ => &[],
  };
  require(
    params
      .keys()
      .all(|k| common.contains(&k.as_str()) || specific.contains(&k.as_str())),
    400,
  )?;
  let url = reqwest::Url::parse(text(&input["parameters"], "base_url")?)
    .map_err(|_| anyhow!(crate::library::Rejected(400)))?;
  require(
    matches!(url.scheme(), "http" | "https")
      && url.host_str().is_some()
      && url.username().is_empty()
      && url.password().is_none()
      && url.query().is_none()
      && url.fragment().is_none(),
    400,
  )?;
  // Network reachability and SSRF policy remain WeKnora's responsibility.
  require(
    input["parameters"]["api_key"].as_str().unwrap_or("").len() <= 16384,
    400,
  )?;
  let provider = input["parameters"]["provider"]
    .as_str()
    .unwrap_or("generic");
  if source == "remote" && provider != "generic" && existing.is_none() {
    require(
      input["parameters"]["api_key"]
        .as_str()
        .is_some_and(|s| !s.trim().is_empty()),
      400,
    )?;
  }
  if let Some(previous) = existing {
    if previous["parameters"]["base_url"] != input["parameters"]["base_url"]
      || previous["parameters"]["provider"] != input["parameters"]["provider"]
    {
      require(
        source == "local"
          || previous["credentials"]["api_key"]["configured"] == false
          || input["parameters"]["api_key"]
            .as_str()
            .is_some_and(|s| !s.trim().is_empty()),
        400,
      )?;
    }
  }
  if kind == "Embedding" {
    let ep = &input["parameters"]["embedding_parameters"];
    require(
      ep["dimension"]
        .as_u64()
        .is_some_and(|n| (1..=65536).contains(&n))
        && ep["truncate_prompt_tokens"]
          .as_u64()
          .is_some_and(|n| (1..=2000000).contains(&n)),
      400,
    )?;
  }
  for field in ["context_window", "max_output_tokens"] {
    if let Some(value) = params.get(field) {
      require(
        value.as_u64().is_some_and(|n| (1..=2000000).contains(&n)),
        400,
      )?;
    }
  }
  if let (Some(context), Some(output)) = (
    params.get("context_window").and_then(Value::as_u64),
    params.get("max_output_tokens").and_then(Value::as_u64),
  ) {
    require(output <= context, 400)?;
  }
  let mut body = json!({"name":name,"display_name":input["display_name"].as_str().unwrap_or(name),"type":kind,"source":source,"description":marker(wid),"parameters":params});
  require(
    body["display_name"].as_str().unwrap_or("").chars().count() <= 200,
    400,
  )?;
  if let Some(previous) = existing {
    if previous["parameters"]["provider"] == body["parameters"]["provider"] {
      for field in ["extra_config", "custom_headers"] {
        if let Some(old) = previous["parameters"][field].as_object() {
          let mut merged = old.clone();
          if let Some(new) = body["parameters"][field].as_object() {
            for (key, value) in new {
              merged.insert(key.clone(), value.clone());
            }
          }
          body["parameters"][field] = json!(merged);
        }
      }
    }
  }
  if body["parameters"]["api_key"]
    .as_str()
    .is_none_or(str::is_empty)
  {
    body["parameters"]
      .as_object_mut()
      .unwrap()
      .remove("api_key");
  }
  Ok(body)
}

pub(crate) async fn execute(
  pool: &PgPool,
  client: &Client,
  session: &Session,
  action: &str,
  input: &Value,
) -> Result<Value> {
  let wid = uuid(input, "workspace_id")?.to_string();
  let workspace = session.workspace(&wid)?;
  let can_manage = workspace["owner_uid"].as_i64() == Some(session.user_id);
  if matches!(action, "model_save" | "model_delete" | "model_test") {
    require(can_manage, 403)?;
  }
  let mut transaction = pool.begin().await?;
  if matches!(action, "model_bind" | "model_delete") {
    let locked: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(781142063)")
      .fetch_one(&mut *transaction)
      .await?;
    require(locked, 409)?;
  }

  let (state, mut settings) = super::library::load(
    &mut transaction,
    matches!(action, "model_bind" | "model_delete"),
  )
  .await?;
  let login = sync::admin_login(client).await?;
  if action == "model_providers" {
    let kind = text(input, "type")?;
    require(TYPES.contains(&kind), 400)?;
    return Ok(
      json!({"providers":sync::admin(client,&login,&format!("models/providers?model_type={kind}"),Method::GET,None).await?["data"]}),
    );
  }
  let response = sync::admin(client, &login, "models", Method::GET, None).await?;
  let all = array(&response["data"])?;
  let models = all
    .iter()
    .filter(|m| visible(m, &wid, &state))
    .collect::<Vec<_>>();
  if action == "model_test" {
    let id = uuid(input, "id")?.to_string();
    let model = models
      .iter()
      .find(|m| m["id"] == id && owned(m, &wid) && m["is_builtin"] != true)
      .ok_or_else(|| anyhow!(crate::library::Rejected(403)))?;
    // Release the catalog transaction before contacting the model provider.
    transaction.commit().await?;
    let available = test_connection(client, &login, model, Some(&id), None).await?;
    return Ok(json!({"available": available}));
  }
  if action == "model_delete" {
    let id = uuid(input, "id")?.to_string();
    validate_delete(all, &wid, &id, &settings)?;
    // WeKnora also rejects references in KBs/agents; local ReRank bindings live in Cloud.
    sync::admin(
      client,
      &login,
      &format!("models/{id}"),
      Method::DELETE,
      None,
    )
    .await?;
    transaction.commit().await?;
    return Ok(json!({"deleted":true}));
  }
  if action == "models" {
    let catalog = sync::admin(client, &login, "models/providers", Method::GET, None).await?;
    let providers = array(&catalog["data"])?
      .iter()
      .map(|p| json!({"value":p["value"],"label":p["label"],"icon":p["icon"]}))
      .collect::<Vec<_>>();
    return Ok(
      json!({"models":models.iter().map(|m|public_model(m,&wid)).collect::<Vec<_>>(),"can_manage":can_manage,"providers":providers}),
    );
  }
  if action == "model_save" {
    let existing = if input["id"].is_string() {
      let id = uuid(input, "id")?.to_string();
      Some(
        *models
          .iter()
          .find(|m| m["id"] == id && owned(m, &wid) && m["is_builtin"] != true)
          .ok_or_else(|| anyhow!(crate::library::Rejected(403)))?,
      )
    } else {
      None
    };
    let body = validate_model(&input["model"], existing, &wid)?;
    // Changing an embedding definition would invalidate every stored vector that uses it.
    if let Some(old) = existing.filter(|m| m["type"] == "Embedding") {
      require(
        old["name"] == body["name"]
          && old["parameters"]["base_url"] == body["parameters"]["base_url"]
          && old["parameters"]["embedding_parameters"]
            == body["parameters"]["embedding_parameters"],
        409,
      )?;
    }

    return save_model(
      client,
      &login,
      &body,
      existing.map(|m| m["id"].as_str().unwrap()),
      &wid,
    )
    .await;
  }
  require(matches!(action, "model_settings" | "model_bind"), 404)?;
  let bid = super::library::base_id(input, &wid)?;
  let info = session.accessible(&wid, &bid, &settings, &state)?;
  require(info["can_manage"] == true, 403)?;
  let group = if bid == wid { "workspaces" } else { "bases" };
  let kb = if bid == wid {
    text(&state["workspaces"][&wid], "id")?
  } else {
    &bid
  };
  let current = sync::admin(
    client,
    &login,
    &format!("knowledge-bases/{kb}"),
    Method::GET,
    None,
  )
  .await?["data"]
    .clone();
  if action == "model_settings" {
    let has_documents = !client
      .entries(text(&state["service"], "api_key")?, kb)
      .await?
      .is_empty();
    return Ok(
      json!({"settings":binding_state(&current,&settings[group][&bid]["model_bindings"]),"embedding_locked":has_documents}),
    );
  }
  let binding = &input["settings"];
  let mut patch = validate_binding(binding, &models)?;
  for field in ["vlm_config", "asr_config"] {
    let mut retained = current[field].as_object().cloned().unwrap_or_default();
    retained.extend(patch[field].as_object().unwrap().clone());
    patch[field] = json!(retained);
  }
  if patch
    .get("embedding_model_id")
    .is_some_and(|id| *id != current["embedding_model_id"])
  {
    require(
      client
        .entries(text(&state["service"], "api_key")?, kb)
        .await?
        .is_empty(),
      409,
    )?;
  }
  sync::admin(
    client,
    &login,
    &format!("knowledge-bases/{kb}"),
    Method::PATCH,
    Some(&patch),
  )
  .await?;
  settings[group][&bid]["model_bindings"] = binding.clone();
  super::library::save(&mut transaction, &settings).await?;
  transaction.commit().await?;
  Ok(json!({"updated":true}))
}

pub(crate) fn validate_delete(
  models: &[Value],
  wid: &str,
  id: &str,
  settings: &Value,
) -> Result<()> {
  require(
    models
      .iter()
      .any(|m| m["id"] == id && owned(m, wid) && m["is_builtin"] != true),
    403,
  )?;
  for group in ["workspaces", "bases"] {
    if let Some(entries) = settings[group].as_object() {
      for entry in entries.values() {
        require(
          !entry["model_bindings"]
            .as_object()
            .is_some_and(|bindings| bindings.values().any(|v| v.as_str() == Some(id))),
          409,
        )?;
      }
    }
  }
  Ok(())
}

fn connection_test_request(
  model: &Value,
  stored_id: Option<&str>,
  api_key: Option<&str>,
) -> Result<(&'static str, Value)> {
  let path = match text(model, "type")? {
    "KnowledgeQA" | "VLLM" => "initialization/remote/check",
    "Embedding" => "initialization/embedding/test",
    "Rerank" => "initialization/rerank/check",
    "ASR" => "initialization/asr/check",
    _ => return Err(anyhow!(crate::library::Rejected(400))),
  };
  let params = &model["parameters"];
  let mut body = json!({
    "modelName": text(model, "name")?,
    "baseUrl": params["base_url"].as_str().unwrap_or_default().trim_end_matches('/').trim_end_matches("/chat/completions"),
    "provider": params["provider"].as_str().unwrap_or("generic"),
    "source": model["source"].as_str().unwrap_or("remote"),
  });
  if let Some(id) = stored_id {
    // Let WeKnora resolve its own stored secrets and provider options by ID.
    body["modelId"] = json!(id);
  }
  if let Some(key) = api_key {
    // Shared chats use the generic OpenAI-compatible adapter at runtime.
    body["provider"] = json!("generic");
    body["apiKey"] = json!(key);
  }
  for (key, name) in [("interface_type", "interfaceType"), ("spec", "spec")] {
    if let Some(value) = params.get(key) {
      body[name] = value.clone();
    }
  }
  if let Some(embedding) = params["embedding_parameters"].as_object() {
    for (key, name) in [
      ("dimension", "dimension"),
      ("supports_dimension_override", "supportsDimensionOverride"),
    ] {
      if let Some(value) = embedding.get(key) {
        body[name] = value.clone();
      }
    }
  }
  Ok((path, body))
}

async fn test_connection(
  client: &Client,
  login: &Value,
  model: &Value,
  stored_id: Option<&str>,
  api_key: Option<&str>,
) -> Result<bool> {
  let (path, body) = connection_test_request(model, stored_id, api_key)?;
  let result = sync::admin(client, login, path, Method::POST, Some(&body)).await?;
  result["data"]["available"]
    .as_bool()
    .ok_or_else(|| anyhow!("invalid model test response"))
}

pub(crate) async fn test_shared_chat(
  client: &Client,
  model: &Value,
  api_key: &str,
) -> Result<bool> {
  let login = sync::admin_login(client).await?;
  test_connection(client, &login, model, None, Some(api_key)).await
}

pub(crate) async fn save_model(
  client: &Client,
  login: &Value,
  body: &Value,
  existing: Option<&str>,
  wid: &str,
) -> Result<Value> {
  let (method, path) = existing
    .map(|id| (Method::PUT, format!("models/{id}")))
    .unwrap_or((Method::POST, "models".into()));
  let updated = sync::admin(client, login, &path, method, Some(body)).await?;
  if existing.is_some() {
    if let Some(key) = body["parameters"]["api_key"].as_str() {
      sync::admin(
        client,
        login,
        &format!("{path}/credentials"),
        Method::PUT,
        Some(&json!({"api_key":key})),
      )
      .await?;
    }
  }
  Ok(json!({"model":public_model(&updated["data"],wid)}))
}

pub(crate) fn binding_state(base: &Value, stored: &Value) -> Value {
  json!({"embedding_model_id":base["embedding_model_id"],"summary_model_id":base["summary_model_id"],
    "vlm_model_id":base["vlm_config"]["model_id"].as_str().unwrap_or(""),
    "asr_model_id":base["asr_config"]["model_id"].as_str().unwrap_or(""),
    "rerank_model_id":stored["rerank_model_id"].as_str().unwrap_or(""),
    "rerank_top_k":stored["rerank_top_k"].as_u64().unwrap_or(10)})
}
pub(crate) fn validate_binding(binding: &Value, models: &[&Value]) -> Result<Value> {
  require(
    binding.as_object().is_some_and(|o| {
      o.keys().all(|k| {
        [
          "embedding_model_id",
          "summary_model_id",
          "vlm_model_id",
          "asr_model_id",
          "rerank_model_id",
          "rerank_top_k",
        ]
        .contains(&k.as_str())
      })
    }),
    400,
  )?;
  let mut patch = json!({});
  for (field, kind) in [
    ("embedding_model_id", "Embedding"),
    ("summary_model_id", "KnowledgeQA"),
    ("vlm_model_id", "VLLM"),
    ("asr_model_id", "ASR"),
    ("rerank_model_id", "Rerank"),
  ] {
    let id = binding[field]
      .as_str()
      .ok_or_else(|| anyhow!(crate::library::Rejected(400)))?;
    require(field != "embedding_model_id" || !id.is_empty(), 400)?;
    if !id.is_empty() {
      require(
        models.iter().any(|m| {
          m["id"] == id && m["type"] == kind && m["status"].as_str().unwrap_or("active") == "active"
        }),
        403,
      )?;
    }
    match field {
      "vlm_model_id" => {
        patch["vlm_config"] = json!({"enabled":!id.is_empty(),"model_id":id});
      },
      "asr_model_id" => {
        patch["asr_config"] = json!({"enabled":!id.is_empty(),"model_id":id});
      },
      "rerank_model_id" => {},
      _ => {
        patch[field] = json!(id);
      },
    }
  }
  require(
    binding["rerank_top_k"]
      .as_u64()
      .is_some_and(|n| (1..=200).contains(&n)),
    400,
  )?;
  Ok(patch)
}

pub(crate) fn search_request(query: &str, binding: &Value) -> Value {
  let mut body = json!({"query_text":query.trim(),"match_count":12,"vector_threshold":0.35});
  if let Some(id) = binding["rerank_model_id"]
    .as_str()
    .filter(|id| !id.is_empty())
  {
    body["rerank"] =
      json!({"enabled":true,"model_id":id,"top_k":binding["rerank_top_k"].as_u64().unwrap_or(10)});
  }
  body
}

#[cfg(test)]
mod tests {
  use super::*;
  fn model(kind: &str) -> Value {
    json!({"id":"model","name":"example","type":kind,"source":"remote","description":marker("workspace"),"parameters":{"base_url":"https://api.example/v1","api_key":"private-key","provider":"generic"}})
  }
  #[test]
  fn tests_saved_models_with_their_capability_and_server_side_credentials() {
    let mut embedding = model("Embedding");
    embedding["parameters"]["embedding_parameters"] =
      json!({"dimension":1024,"supports_dimension_override":true});
    let (path, body) = connection_test_request(&embedding, Some("saved-id"), None).unwrap();
    assert_eq!(path, "initialization/embedding/test");
    assert_eq!(body["modelId"], "saved-id");
    assert_eq!(body["dimension"], 1024);
    assert!(body.get("apiKey").is_none());
    let (path, body) =
      connection_test_request(&model("KnowledgeQA"), None, Some("saved-key")).unwrap();
    assert_eq!(path, "initialization/remote/check");
    assert_eq!(body["apiKey"], "saved-key");
    assert!(body.get("modelId").is_none());
  }
  #[test]
  fn model_categories_validate_their_own_fields_and_keep_secrets_out_of_responses() {
    let mut embedding = model("Embedding");
    embedding["parameters"]["embedding_parameters"] =
      json!({"dimension":1024,"truncate_prompt_tokens":8192});
    assert!(validate_model(&embedding, None, "workspace").is_ok());
    embedding["parameters"]["context_window"] = json!(128000);
    assert!(validate_model(&embedding, None, "workspace").is_err());
    let mut rerank = model("Rerank");
    rerank["parameters"]["embedding_parameters"] = json!({"dimension":1024});
    assert!(validate_model(&rerank, None, "workspace").is_err());
    let mut chat = model("KnowledgeQA");
    chat["parameters"]["context_window"] = json!(1000);
    chat["parameters"]["max_output_tokens"] = json!(2000);
    assert!(validate_model(&chat, None, "workspace").is_err());
    chat["parameters"]["max_output_tokens"] = json!(500);
    chat["parameters"]["custom_headers"] = json!({"X-Key":"secret"});
    chat["parameters"]["spec"] = json!({"reasoning":true,"compat":{"key":"secret"}});
    let public = public_model(&chat, "workspace").to_string();
    assert!(
      !public.contains("private-key")
        && !public.contains("secret")
        && !public.contains("custom_headers")
    );
    assert!(validate_model(&chat, None, "workspace").is_ok());
    let previous = chat.clone();
    chat["parameters"]["base_url"] = json!("https://different.example/v1");
    chat["parameters"]
      .as_object_mut()
      .unwrap()
      .remove("api_key");
    assert!(validate_model(&chat, Some(&previous), "workspace").is_err());
  }
  #[test]
  fn workspace_catalog_hides_foreign_models_and_bindings_reject_wrong_categories() {
    let state = json!({"workspaces":{"workspace":{"embedding_proxy":{"id":"internal-embedding"}}}});
    let own = model("Embedding");
    let mut other = own.clone();
    other["description"] = json!(marker("other"));
    assert!(visible(&own, "workspace", &state));
    assert!(!visible(&other, "workspace", &state));
    let mut binding = json!({"embedding_model_id":"model","summary_model_id":"","vlm_model_id":"","asr_model_id":"","rerank_model_id":"","rerank_top_k":10});
    assert!(validate_binding(&binding, &[&own]).is_ok());
    binding["rerank_model_id"] = json!("model");
    assert!(validate_binding(&binding, &[&own]).is_err());
    binding["rerank_model_id"] = json!("");
    binding["embedding_model_id"] = json!("foreign-id");
    assert!(validate_binding(&binding, &[&own]).is_err());
    let request = search_request(
      "question",
      &json!({"rerank_model_id":"real-rerank","rerank_top_k":7}),
    );
    assert_eq!(
      request["rerank"],
      json!({"enabled":true,"model_id":"real-rerank","top_k":7})
    );
    assert!(search_request("question", &Value::Null)
      .get("rerank")
      .is_none());
  }

  #[test]
  fn shared_library_models_are_readonly_without_becoming_agent_builtins() {
    let own = model("KnowledgeQA");
    let public = public_model(&own, "workspace");
    assert_eq!(public["can_edit"], true);
    assert_eq!(public["is_builtin"], false);
    let mut shared = own.clone();
    shared["description"] = json!("");
    let public = public_model(&shared, "workspace");
    assert_eq!(public["can_edit"], false);
    assert_eq!(public["is_builtin"], false);
    assert!(visible(&shared, "workspace", &Value::Null));
    shared["is_builtin"] = json!(true);
    let public = public_model(&shared, "workspace");
    assert_eq!(public["is_builtin"], true);
    assert_eq!(public["can_edit"], false);
    assert!(public["parameters"].get("api_key").is_none());
  }
}
