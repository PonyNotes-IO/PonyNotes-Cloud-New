#[path = "agent_sync.rs"]
mod agent_sync;

use crate::notes::Notes;
use crate::{array, managed, not_found, optional_array, text, Client};
use anyhow::{anyhow, ensure, Context, Result};
use reqwest::Method;
use serde_json::{json, Value};
use sqlx::{PgConnection, PgPool};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

/// Defaults off. Existing deployments keep working until migration explicitly enables it.
pub async fn run(pool: PgPool) {
  if !crate::enabled() {
    return;
  }
  let client = match Client::from_env() {
    Ok(client) => client,
    Err(_) => {
      tracing::error!("WeKnora client configuration failed");
      return;
    },
  };
  loop {
    match run_once(&pool, &client).await {
      Ok(report) => tracing::info!(
        workspaces = report["workspaces"].as_array().map(Vec::len),
        errors = report["errors"].as_array().map(Vec::len),
        "WeKnora sync completed"
      ),
      Err(_) => tracing::error!("WeKnora sync failed; existing knowledge preserved"),
    }
    // ponytail: keep the existing five-minute scan; use the existing Redis queue if scans outgrow it.
    tokio::time::sleep(Duration::from_secs(300)).await;
  }
}

pub async fn run_once(pool: &PgPool, client: &Client) -> Result<Value> {
  let mut connection = pool.acquire().await?;
  let locked: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock(781142063)")
    .fetch_one(&mut *connection)
    .await?;
  if !locked {
    return Ok(json!({"workspaces":[],"errors":[],"skipped":"already_running"}));
  }
  let result = cycle(&mut connection, client).await;
  // Close rather than pool a session whose advisory lock could not be released.
  if sqlx::query("SELECT pg_advisory_unlock(781142063)")
    .execute(&mut *connection)
    .await
    .is_err()
  {
    connection.close().await?;
  }
  result
}

pub(crate) async fn persist(
  connection: &mut PgConnection,
  group: &str,
  id: Option<&str>,
  record: &Value,
) -> Result<()> {
  let result = if let Some(id) = id {
    sqlx::query("UPDATE af_weknora_sync_state SET state=jsonb_set(state,ARRAY[$1,$2],$3),updated_at=now() WHERE singleton")
      .bind(group).bind(id).bind(record).execute(connection).await?
  } else {
    sqlx::query("UPDATE af_weknora_sync_state SET state=jsonb_set(state,ARRAY[$1],$2),updated_at=now() WHERE singleton")
      .bind(group).bind(record).execute(connection).await?
  };
  ensure!(result.rows_affected() == 1, "knowledge state missing");
  Ok(())
}

fn user_token(user: &Value) -> Result<String> {
  let secret =
    std::env::var("APPFLOWY_GOTRUE_JWT_SECRET").context("missing Cloud JWT configuration")?;
  let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
  jsonwebtoken::encode(&jsonwebtoken::Header::default(),&json!({
    "sub":text(user,"uuid")?,"aud":"authenticated","iat":now,"exp":now+600,
    "email":user["email"].as_str().unwrap_or(""),"phone":"","role":"authenticated","app_metadata":{},"user_metadata":{}
  }),&jsonwebtoken::EncodingKey::from_secret(secret.as_bytes())).map_err(|_|anyhow!("cannot sign user token"))
}

async fn admin_login(client: &Client) -> Result<Value> {
  let path = std::env::var("PONYNOTES_WEKNORA_ADMIN_FILE").context("missing administrator file")?;
  let credentials: Value = serde_json::from_slice(&tokio::fs::read(path).await?)?;
  client
    .request(
      &(client.base.clone() + "auth/login"),
      Method::POST,
      None,
      Some(&credentials),
    )
    .await
}
async fn admin(
  client: &Client,
  login: &Value,
  path: &str,
  method: Method,
  payload: Option<&Value>,
) -> Result<Value> {
  client
    .request(
      &(client.base.clone() + path),
      method,
      Some((
        "Authorization",
        &format!("Bearer {}", text(login, "token")?),
      )),
      payload,
    )
    .await
}
fn preferred_model(models: &[Value], kind: &str) -> String {
  let active = models
    .iter()
    .filter(|m| m["type"] == kind && m["status"].as_str().unwrap_or("active") == "active")
    .collect::<Vec<_>>();
  let defaults = active
    .iter()
    .copied()
    .filter(|m| m["is_default"] == true)
    .collect::<Vec<_>>();
  let choices = if defaults.is_empty() {
    &active
  } else {
    &defaults
  };
  if choices.len() == 1 {
    choices[0]["id"].as_str().unwrap_or("").to_owned()
  } else {
    String::new()
  }
}
async fn provision(
  client: &Client,
  connection: &mut PgConnection,
  state: &mut Value,
) -> Result<Value> {
  let login = admin_login(client).await?;
  let mut service = state["service"].clone();
  if service.is_null() {
    service = json!({"tenant_id":login["active_tenant"]["id"]});
  }
  ensure!(
    !service["tenant_id"].is_null() && service["tenant_id"] == login["active_tenant"]["id"],
    "service tenant mismatch"
  );
  let path = format!("tenants/{}/api-keys", display_id(&service["tenant_id"])?);
  let keys = admin(client, &login, &path, Method::GET, None).await?;
  let scope = json!({"name":"PonyNotes workspaces","full_access":false,"knowledge_base_ids":[],
    "capabilities":["retrieve","ingest","manage_kbs","manage_datasources"]});
  let key = optional_array(&keys["data"])?
    .into_iter()
    .find(|k| k["name"] == "PonyNotes workspaces");
  let known = key
    .and_then(|_| service["api_key"].as_str())
    .map(str::to_owned);
  let key = if let Some(key) = key {
    let capabilities = optional_array(&key["capabilities"])?
      .into_iter()
      .filter_map(Value::as_str)
      .collect::<HashSet<_>>();
    if key["full_access"] == true
      || key["knowledge_base_ids"]
        .as_array()
        .is_some_and(|a| !a.is_empty())
      || capabilities != HashSet::from(["retrieve", "ingest", "manage_kbs", "manage_datasources"])
    {
      admin(
        client,
        &login,
        &format!("{path}/{}", display_id(&key["id"])?),
        Method::PUT,
        Some(&scope),
      )
      .await?["data"]
        .clone()
    } else {
      key.clone()
    }
  } else {
    admin(client, &login, &path, Method::POST, Some(&scope)).await?["data"].clone()
  };
  service["api_key"] = json!(known
    .or_else(|| key["token"]
      .as_str()
      .or_else(|| key["api_key"].as_str())
      .map(str::to_owned))
    .ok_or_else(|| anyhow!("missing service key"))?);
  let response = admin(client, &login, "models", Method::GET, None).await?;
  let models = optional_array(&response["data"])?
    .into_iter()
    .cloned()
    .collect::<Vec<_>>();
  let embedding = models
    .iter()
    .filter(|m| {
      !m["description"]
        .as_str()
        .unwrap_or("")
        .starts_with("PonyNotes workspace embedding ")
    })
    .cloned()
    .collect::<Vec<_>>();
  service["embedding_model_id"] = json!(preferred_model(&embedding, "Embedding"));
  service["summary_model_id"] = json!(preferred_model(&models, "KnowledgeQA"));
  // Cloud receives only the selectable metadata, never the administrator's credentials.
  service["parse_models"] = json!(models.iter()
    .filter(|m| m["status"].as_str().unwrap_or("active") == "active")
    .filter(|m| matches!(m["type"].as_str(), Some("VLLM" | "ASR")))
    .map(|m| json!({"id":m["id"],"name":m["display_name"].as_str().filter(|s|!s.is_empty()).or_else(||m["name"].as_str()).unwrap_or(""),"type":m["type"]}))
    .collect::<Vec<_>>());
  if let Ok(response) = admin(client, &login, "system/parser-engines", Method::GET, None).await {
    service["parse_engines"] = json!(array(&response["data"])?
      .iter()
      .filter(|e| e["Available"] == true
        && e["FileTypes"]
          .as_array()
          .is_some_and(|types| types.iter().any(|t| t == "html")))
      .map(|e| json!({"id":e["Name"],"name":e["Name"]}))
      .collect::<Vec<_>>());
  }
  persist(connection, "service", None, &service).await?;
  state["service"] = service;
  Ok(login)
}
fn display_id(value: &Value) -> Result<String> {
  if let Some(id) = value.as_str() {
    Ok(id.to_owned())
  } else if value.is_number() {
    Ok(value.to_string())
  } else {
    anyhow::bail!("invalid identifier")
  }
}

async fn workspace_embedding(
  client: &Client,
  connection: &mut PgConnection,
  login: &Value,
  wid: &str,
  mapping: &mut Value,
  source: &str,
) -> Result<String> {
  ensure!(!source.is_empty(), "embedding model not configured");
  if mapping["embedding_proxy"].is_null() {
    let model = admin(
      client,
      login,
      &format!("models/{source}"),
      Method::GET,
      None,
    )
    .await?;
    let model = &model["data"];
    ensure!(
      model["type"] == "Embedding" && model["source"] == "local",
      "workspace accounting requires configured local embedding model"
    );
    mapping["embedding_proxy"] = json!({"token":format!("{}{}",Uuid::new_v4().simple(),Uuid::new_v4().simple()),
      "name":text(model,"name")?,"parameters":model["parameters"]["embedding_parameters"],"source_id":source,"ready":false});
    persist(connection, "workspaces", Some(wid), mapping).await?;
  }
  let proxy = &mapping["embedding_proxy"];
  let url = format!(
    "{}/{wid}/v1",
    std::env::var("PONYNOTES_WEKNORA_EMBEDDING_URL")
      .unwrap_or_else(|_| "http://appflowy_cloud:8000/internal/knowledge".into())
      .trim_end_matches('/')
  );
  let description = format!("PonyNotes workspace embedding {wid}");
  let response = admin(client, login, "models", Method::GET, None).await?;
  let models = optional_array(&response["data"])?
    .into_iter()
    .filter(|m| m["description"] == description)
    .collect::<Vec<_>>();
  ensure!(models.len() <= 1, "duplicate workspace embedding models");
  let parameters = json!({"base_url":url,"api_key":proxy["token"],"provider":"generic","embedding_parameters":proxy["parameters"]});
  let model = if let Some(model) = models.first() {
    ensure!(
      model["type"] == "Embedding"
        && model["source"] == "remote"
        && model["name"] == proxy["name"]
        && model["parameters"]["embedding_parameters"] == proxy["parameters"],
      "workspace embedding mismatch"
    );
    if model["parameters"]["base_url"] != url {
      // Preserve the model ID/dimensions and all existing vectors while moving its callback off Python.
      admin(client,login,&format!("models/{}",text(model,"id")?),Method::PUT,
        Some(&json!({"name":proxy["name"],"type":"Embedding","source":"remote","description":description,"parameters":parameters}))).await?["data"].clone()
    } else {
      (*model).clone()
    }
  } else {
    admin(client,login,"models",Method::POST,Some(&json!({"name":proxy["name"],"type":"Embedding","source":"remote","description":description,"parameters":parameters}))).await?["data"].clone()
  };
  ensure!(
    model["parameters"]["base_url"] == url
      && model["parameters"]["embedding_parameters"] == proxy["parameters"],
    "embedding callback update failed"
  );
  let id = text(&model, "id")?.to_owned();
  if let Some(previous) = proxy["id"].as_str() {
    ensure!(previous == id, "embedding model ID changed");
  }
  mapping["embedding_proxy"]["id"] = json!(id);
  mapping["embedding_proxy"]["ready"] = json!(true);
  persist(connection, "workspaces", Some(wid), mapping).await?;
  Ok(id)
}

pub(crate) async fn reconcile(
  client: &Client,
  key: &str,
  kb: &Value,
  desired: &Notes,
) -> Result<Value> {
  let id = text(kb, "id")?;
  let mut existing = HashMap::new();
  let mut duplicates = Vec::new();
  for entry in client.entries(key, id).await? {
    let channel = entry["channel"].as_str().unwrap_or("").to_owned();
    if !managed(&channel) || entry["parse_status"] == "deleting" {
      continue;
    }
    if existing.contains_key(&channel) {
      duplicates.push(entry);
    } else {
      existing.insert(channel, entry);
    }
  }
  let status = if kb["embedding_model_id"]
    .as_str()
    .is_some_and(|s| !s.is_empty())
  {
    "publish"
  } else {
    "draft"
  };
  let mut counts = json!({"created":0,"updated":0,"unchanged":0,"deleted":0});
  for (channel, content) in desired {
    let payload = json!({"title":content["title"],"content":content["content"],"status":status});
    if let Some(entry) = existing.get(channel) {
      let unchanged = entry["title"] == payload["title"]
        && entry["metadata"]["content"] == payload["content"]
        && entry["parse_status"] != "failed"
        && ((status == "draft" && entry["parse_status"] == "draft")
          || (status == "publish" && entry["parse_status"] != "draft"));
      if unchanged {
        increment(&mut counts, "unchanged");
        continue;
      }
      client
        .weknora(
          key,
          &format!("knowledge/manual/{}", text(entry, "id")?),
          Method::PUT,
          Some(&payload),
        )
        .await?;
      increment(&mut counts, "updated");
    } else {
      let mut payload = payload;
      payload["channel"] = json!(channel);
      client
        .weknora(
          key,
          &format!("knowledge-bases/{id}/knowledge/manual"),
          Method::POST,
          Some(&payload),
        )
        .await?;
      increment(&mut counts, "created");
    }
  }
  // No pruning until the source scan and every destination write have succeeded.
  for entry in existing
    .iter()
    .filter(|(channel, _)| !desired.contains_key(*channel))
    .map(|(_, v)| v)
    .chain(duplicates.iter())
  {
    client
      .weknora(
        key,
        &format!("knowledge/{}", text(entry, "id")?),
        Method::DELETE,
        None,
      )
      .await?;
    increment(&mut counts, "deleted");
  }
  Ok(counts)
}

fn increment(counts: &mut Value, key: &str) {
  counts[key] = json!(counts[key].as_u64().unwrap_or(0) + 1);
}

async fn sync_workspace(
  connection: &mut PgConnection,
  client: &Client,
  login: &Value,
  service: &Value,
  workspace: &Value,
  mapping: &mut Value,
) -> Result<Value> {
  let wid = text(workspace, "workspace_id")?;
  let members = array(&workspace["members"])?;
  ensure!(
    !members.is_empty(),
    "workspace has no active members; preserving knowledge"
  );
  // ponytail: serial member scans retain the existing five-minute schedule; queue deltas if needed.
  // A complete scan of EVERY active non-Guest must succeed before reconciliation can prune.
  let mut desired = Notes::new();
  for user in members {
    let notes = client
      .scan(&user_token(user)?, wid, text(user, "uuid")?)
      .await?;
    for (channel, note) in notes {
      desired.entry(channel).or_insert(note);
    }
  }
  if mapping.is_null() {
    *mapping = json!({"id":Uuid::new_v5(&Uuid::NAMESPACE_URL,format!("ponynotes:workspace:{wid}").as_bytes()),"notes":{},"ready":false});
    persist(connection, "workspaces", Some(wid), mapping).await?;
  }
  let key = text(service, "api_key")?;
  let id = text(mapping, "id")?.to_owned();
  let path = format!("knowledge-bases/{id}");
  let mut kb = match client.weknora(key, &path, Method::GET, None).await {
    Ok(response) => response["data"].clone(),
    Err(error) if not_found(&error) => Value::Null,
    Err(error) => return Err(error),
  };
  let source = kb["embedding_model_id"]
    .as_str()
    .filter(|s| !s.is_empty())
    .or_else(|| service["embedding_model_id"].as_str())
    .unwrap_or("")
    .to_owned();
  let embedding = workspace_embedding(client, connection, login, wid, mapping, &source).await?;
  if kb.is_null() {
    kb = client.weknora(key,"knowledge-bases",Method::POST,Some(&json!({"id":id,"name":format!("小马笔记 · {}",text(workspace,"workspace_name")?),
      "type":"document","description":"工作空间知识库；小马云端笔记自动同步。","embedding_model_id":embedding,
      "summary_model_id":service["summary_model_id"].as_str().unwrap_or("")}))).await?["data"].clone();
  } else {
    let mut updates = json!({});
    if kb["embedding_model_id"] != embedding {
      ensure!(
        kb["embedding_model_id"].as_str().unwrap_or("").is_empty()
          || client.entries(key, &id).await?.is_empty(),
        "existing KB requires reindex before changing embedding model"
      );
      updates["embedding_model_id"] = json!(embedding);
    }
    if kb["summary_model_id"].as_str().unwrap_or("").is_empty()
      && !service["summary_model_id"]
        .as_str()
        .unwrap_or("")
        .is_empty()
    {
      updates["summary_model_id"] = service["summary_model_id"].clone();
    }
    if updates != json!({}) {
      kb = client
        .weknora(key, &path, Method::PATCH, Some(&updates))
        .await?["data"]
        .clone();
    }
  }
  ensure!(
    kb["id"] == id && kb["embedding_model_id"] == embedding,
    "KB mapping mismatch"
  );
  let counts = reconcile(client, key, &kb, &desired).await?;
  mapping["notes"] = Value::Object(desired.keys().map(|k| (k.clone(), json!({}))).collect());
  mapping["ready"] = json!(true);
  persist(connection, "workspaces", Some(wid), mapping).await?;
  Ok(json!({"workspace":wid,"members":members.len(),"parts":desired.len(),"sync":counts}))
}

async fn cycle(connection: &mut PgConnection, client: &Client) -> Result<Value> {
  let mut state: Value =
    sqlx::query_scalar("SELECT state FROM af_weknora_sync_state WHERE singleton")
      .fetch_one(&mut *connection)
      .await?;
  // Deploying binaries must not start a new writer before the legacy mappings/settings are imported.
  ensure!(
    matches!(state["native_library_version"].as_u64(), Some(0 | 1)),
    "native knowledge migration required"
  );
  let workspaces: Vec<Value> = sqlx::query_scalar(
    "SELECT jsonb_build_object('workspace_id',w.workspace_id,'workspace_name',w.workspace_name, \
     'members',coalesce((SELECT jsonb_agg(jsonb_build_object('uuid',u.uuid,'email',u.email) ORDER BY (u.uid=w.owner_uid) DESC,u.uuid) \
     FROM af_workspace_member m JOIN af_user u ON u.uid=m.uid JOIN auth.users a ON a.id=u.uuid \
     WHERE m.workspace_id=w.workspace_id AND m.role_id<>3 AND a.deleted_at IS NULL \
     AND (a.banned_until IS NULL OR a.banned_until<now())),'[]'::jsonb)) \
     FROM af_workspace w WHERE w.deleted_at IS NULL AND coalesce(w.is_initialized,true)")
    .fetch_all(&mut *connection).await?;
  let login = provision(client, connection, &mut state).await?;
  let service = state["service"].clone();
  let mut report = json!({"at":SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),"workspaces":[],"errors":[]});
  let mut active = HashSet::new();
  for workspace in workspaces {
    let wid = text(&workspace, "workspace_id")?.to_owned();
    Uuid::parse_str(&wid)?;
    active.insert(wid.clone());
    let mut mapping = state["workspaces"][&wid].clone();
    match sync_workspace(
      connection,
      client,
      &login,
      &service,
      &workspace,
      &mut mapping,
    )
    .await
    {
      Ok(result) => report["workspaces"].as_array_mut().unwrap().push(result),
      Err(_) => report["errors"]
        .as_array_mut()
        .unwrap()
        .push(json!({"workspace":wid,"error":"sync_failed"})),
    }
  }
  // Public library indexes are shared; isolated Agent indexes retain their existing identities.
  for (wid, mapping) in state["workspaces"]
    .as_object()
    .ok_or_else(|| anyhow!("invalid mappings"))?
  {
    if active.contains(wid) {
      continue;
    }
    let result = client
      .weknora(
        text(&service, "api_key")?,
        &format!("knowledge-bases/{}", text(mapping, "id")?),
        Method::DELETE,
        None,
      )
      .await;
    if result.is_ok() || result.as_ref().err().is_some_and(not_found) {
      sqlx::query("UPDATE af_weknora_sync_state SET state=state #- ARRAY['workspaces',$1],updated_at=now() WHERE singleton")
        .bind(wid).execute(&mut *connection).await?;
    } else {
      report["errors"]
        .as_array_mut()
        .unwrap()
        .push(json!({"workspace":wid,"error":"cleanup_failed"}));
    }
  }
  // Reuse the deployed Rust Agent sync under this same advisory lock.
  match agent_sync::run(connection, client).await {
    Ok(agent) => {
      if let Some(errors) = agent["errors"].as_array() {
        report["errors"]
          .as_array_mut()
          .unwrap()
          .extend(errors.iter().cloned());
      }
      report["agent"] = agent;
    },
    Err(_) => report["errors"]
      .as_array_mut()
      .unwrap()
      .push(json!({"error":"agent_sync_failed"})),
  }
  sqlx::query("UPDATE af_weknora_sync_state SET last_run=$1,updated_at=now() WHERE singleton")
    .bind(&report)
    .execute(connection)
    .await?;
  Ok(report)
}
