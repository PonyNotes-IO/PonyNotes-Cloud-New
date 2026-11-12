//! Cloud-authenticated knowledge management. Only server-side mappings select a tenant/KB.
use crate::{array, not_found, optional_array, text, Client, UpstreamStatus};
use anyhow::{anyhow, ensure, Result};
use reqwest::Method;
use serde_json::{json, Value};
use sqlx::{PgConnection, PgPool};
use std::collections::HashSet;
use uuid::Uuid;

pub const ACTIONS: &[&str] = &[
  "bases",
  "create",
  "visibility",
  "favorite",
  "documents",
  "document",
  "sources",
  "wiki",
  "wiki_page",
  "search",
  "web_import_options",
  "import_url",
];

#[derive(Debug)]
pub struct Rejected(pub u16);
impl std::fmt::Display for Rejected {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(f, "knowledge request rejected ({})", self.0)
  }
}
impl std::error::Error for Rejected {}
pub(crate) fn require(condition: bool, status: u16) -> Result<()> {
  ensure!(condition, Rejected(status));
  Ok(())
}
pub fn status(error: &anyhow::Error) -> u16 {
  if let Some(error) = error.downcast_ref::<Rejected>() {
    error.0
  } else if let Some(error) = error.downcast_ref::<UpstreamStatus>() {
    match error.0.as_u16() {
      status @ (400 | 401 | 403 | 404 | 409) => status,
      _ => 503,
    }
  } else {
    503
  }
}

pub fn uuid(payload: &Value, field: &str) -> Result<Uuid> {
  payload[field]
    .as_str()
    .and_then(|s| Uuid::parse_str(s).ok())
    .ok_or_else(|| anyhow!(Rejected(400)))
}

pub struct Session {
  pub(crate) uid: Uuid,
  pub(crate) user_id: i64,
  pub(crate) workspaces: Vec<Value>,
}
impl Session {
  pub async fn authenticate(client: &Client, uid: Uuid, user_id: i64, token: &str) -> Result<Self> {
    require(!token.is_empty() && token.len() <= 16384, 401)?;
    // JWT verification is done by Cloud; GoTrue also rejects deleted/banned sessions.
    let user = client
      .request(
        &format!("{}/gotrue/user", client.cloud),
        Method::GET,
        Some(("Authorization", &format!("Bearer {token}"))),
        None,
      )
      .await
      .map_err(|error| {
        if error
          .downcast_ref::<UpstreamStatus>()
          .is_some_and(|e| matches!(e.0.as_u16(), 401 | 403))
        {
          anyhow!(Rejected(401))
        } else {
          error
        }
      })?;
    require(user["id"] == uid.to_string(), 401)?;
    let workspaces = client.cloud(token, "workspace?include_role=true").await?;
    Ok(Self {
      uid,
      user_id,
      workspaces: array(&workspaces)?.clone(),
    })
  }

  fn workspace(&self, wid: &str) -> Result<&Value> {
    self
      .workspaces
      .iter()
      .find(|w| w["workspace_id"] == wid)
      .ok_or_else(|| anyhow!(Rejected(403)))
  }

  pub(crate) fn base_info(
    &self,
    workspace: &Value,
    bid: &str,
    config: &Value,
    state: &Value,
  ) -> Option<Value> {
    let wid = workspace["workspace_id"].as_str()?;
    let custom = bid != wid;
    let record = if custom {
      &config["bases"][bid]
    } else {
      &config["workspaces"][wid]
    };
    if custom && record["workspace_id"] != wid {
      return None;
    }
    let visibility = record["visibility"].as_str().unwrap_or("shared");
    let can_manage = if custom {
      record["owner_uuid"] == self.uid.to_string()
    } else {
      workspace["owner_uid"].as_i64() == Some(self.user_id)
    };
    if visibility != "shared" && !can_manage {
      return None;
    }
    Some(json!({"id":bid,"workspace_id":wid,
      "name":if custom { &record["name"] } else { &workspace["workspace_name"] },
      "visibility":visibility,"can_manage":can_manage,
      "is_favorite":config["favorites"][self.uid.to_string()].as_array().is_some_and(|a| a.iter().any(|id| id == bid)),
      "ready":if custom {record["ready"] == true} else {state["workspaces"][wid]["ready"] == true}}))
  }

  fn accessible(&self, wid: &str, bid: &str, config: &Value, state: &Value) -> Result<Value> {
    self
      .base_info(self.workspace(wid)?, bid, config, state)
      .ok_or_else(|| anyhow!(Rejected(403)))
  }

  fn can_write(&self, wid: &str) -> Result<()> {
    require(
      matches!(
        self.workspace(wid)?["role"].as_str(),
        Some("Owner" | "Member")
      ),
      403,
    )
  }
}

async fn load(connection: &mut PgConnection, locked: bool) -> Result<(Value, Value)> {
  // ponytail: serialize settings on one row; split per-base rows if concurrent writes become frequent.
  let query = if locked {
    "SELECT state,settings FROM af_weknora_sync_state WHERE singleton FOR UPDATE"
  } else {
    "SELECT state,settings FROM af_weknora_sync_state WHERE singleton"
  };
  let (state, settings): (Value, Value) = sqlx::query_as(query).fetch_one(connection).await?;
  require(
    matches!(state["native_library_version"].as_u64(), Some(0 | 1)),
    503,
  )?;
  Ok((state, settings))
}
async fn save(connection: &mut PgConnection, settings: &Value) -> Result<()> {
  sqlx::query("UPDATE af_weknora_sync_state SET settings=$1,updated_at=now() WHERE singleton")
    .bind(settings)
    .execute(connection)
    .await?;
  Ok(())
}

pub async fn execute(
  pool: &PgPool,
  client: &Client,
  session: &Session,
  token: &str,
  action: &str,
  payload: &Value,
) -> Result<Value> {
  require(ACTIONS.contains(&action), 404)?;
  require(payload.is_object(), 400)?;
  if action == "create" {
    return create(pool, client, session, payload).await;
  }
  if matches!(action, "visibility" | "favorite") {
    let wid = uuid(payload, "workspace_id")?.to_string();
    let bid = base_id(payload, &wid)?;
    // Row locks work across Cloud replicas; recheck permissions under the same lock.
    let mut transaction = pool.begin().await?;
    let (state, mut config) = load(&mut transaction, true).await?;
    let info = session.accessible(&wid, &bid, &config, &state)?;
    if action == "visibility" {
      require(info["can_manage"] == true, 403)?;
      require(
        matches!(payload["visibility"].as_str(), Some("private" | "shared")),
        400,
      )?;
      let group = if bid == wid { "workspaces" } else { "bases" };
      config[group][&bid]["visibility"] = payload["visibility"].clone();
    } else {
      let favorite = payload["is_favorite"]
        .as_bool()
        .ok_or_else(|| anyhow!(Rejected(400)))?;
      let uid = session.uid.to_string();
      let mut favorites = optional_array(&config["favorites"][&uid])?
        .into_iter()
        .cloned()
        .collect::<Vec<_>>();
      favorites.retain(|id| id != &bid);
      if favorite {
        favorites.push(json!(bid));
      }
      favorites.sort_by_key(Value::to_string);
      config["favorites"][uid] = json!(favorites);
    }
    save(&mut transaction, &config).await?;
    transaction.commit().await?;
    return Ok(json!({"updated":true}));
  }
  let mut connection = pool.acquire().await?;
  let (state, config) = load(&mut connection, false).await?;
  drop(connection);
  read(client, session, token, action, payload, &state, &config).await
}

fn base_id(payload: &Value, wid: &str) -> Result<String> {
  if payload.get("base_id").is_none() {
    Ok(wid.to_owned())
  } else {
    Ok(uuid(payload, "base_id")?.to_string())
  }
}

async fn create(
  pool: &PgPool,
  client: &Client,
  session: &Session,
  payload: &Value,
) -> Result<Value> {
  let wid = uuid(payload, "workspace_id")?.to_string();
  session.can_write(&wid)?;
  let name = payload["name"]
    .as_str()
    .ok_or_else(|| anyhow!(Rejected(400)))?;
  require(
    (1..=100).contains(&name.trim().chars().count()) && !name.chars().any(char::is_control),
    400,
  )?;
  require(
    matches!(payload["visibility"].as_str(), Some("private" | "shared")),
    400,
  )?;
  let request_id = uuid(payload, "request_id")?;
  let bid = Uuid::new_v5(
    &Uuid::NAMESPACE_URL,
    format!("ponynotes:base:{}:{wid}:{request_id}", session.uid).as_bytes(),
  )
  .to_string();
  let expected = json!({"id":bid,"workspace_id":wid,"owner_uuid":session.uid,"name":name.trim(),"visibility":payload["visibility"]});
  // Persist pending metadata first, so failed/lost remote responses retain both ID and owner.
  let mut transaction = pool.begin().await?;
  let (state, mut config) = load(&mut transaction, true).await?;
  if let Some(record) = config["bases"].get(&bid) {
    require(
      expected
        .as_object()
        .unwrap()
        .iter()
        .all(|(k, v)| record[k] == *v),
      400,
    )?;
  } else {
    require(
      state["workspaces"][&wid]["embedding_proxy"]["ready"] == true,
      503,
    )?;
    config["bases"][&bid] = expected;
    config["bases"][&bid]["ready"] = json!(false);
    save(&mut transaction, &config).await?;
  }
  transaction.commit().await?;
  let mut transaction = pool.begin().await?;
  let (state, mut config) = load(&mut transaction, true).await?;
  let info = session.accessible(&wid, &bid, &config, &state)?;
  if info["ready"] != true {
    let proxy = &state["workspaces"][&wid]["embedding_proxy"];
    require(proxy["ready"] == true, 503)?;
    let key = text(&state["service"], "api_key")?;
    let kb = match client.weknora(key, &format!("knowledge-bases/{bid}"), Method::GET, None).await {
      Ok(response) => response["data"].clone(),
      Err(error) if not_found(&error) => client.weknora(key, "knowledge-bases", Method::POST,
        Some(&json!({"id":bid,"name":config["bases"][&bid]["name"],"type":"document",
          "embedding_model_id":proxy["id"],"summary_model_id":state["service"]["summary_model_id"].as_str().unwrap_or(""),
          "description":"小马笔记知识库"}))).await?["data"].clone(),
      Err(error) => return Err(error),
    };
    ensure!(
      kb["id"] == bid && kb["embedding_model_id"] == proxy["id"],
      "knowledge base binding mismatch"
    );
    config["bases"][&bid]["ready"] = json!(true);
    save(&mut transaction, &config).await?;
  }
  let info = session.accessible(&wid, &bid, &config, &state)?;
  transaction.commit().await?;
  Ok(json!({"base":info}))
}

pub(crate) async fn read(
  client: &Client,
  session: &Session,
  token: &str,
  action: &str,
  payload: &Value,
  state: &Value,
  config: &Value,
) -> Result<Value> {
  if action == "bases" {
    let mut bases = Vec::new();
    for workspace in &session.workspaces {
      let wid = text(workspace, "workspace_id")?;
      for bid in std::iter::once(wid).chain(
        config["bases"]
          .as_object()
          .into_iter()
          .flat_map(|m| m.keys().map(String::as_str)),
      ) {
        if let Some(info) = session.base_info(workspace, bid, config, state) {
          bases.push(info);
        }
      }
    }
    return Ok(json!({"bases":bases}));
  }
  let wid = uuid(payload, "workspace_id")?.to_string();
  let bid = base_id(payload, &wid)?;
  let info = session.accessible(&wid, &bid, config, state)?;
  if matches!(action, "web_import_options" | "import_url") {
    session.can_write(&wid)?;
  }
  if action == "search" {
    let query = payload["query"]
      .as_str()
      .ok_or_else(|| anyhow!(Rejected(400)))?;
    require((1..=2000).contains(&query.trim().chars().count()), 400)?;
  }
  let mapping = if bid == wid {
    &state["workspaces"][&wid]
  } else {
    &config["bases"][&bid]
  };
  if mapping["ready"] != true {
    require(!matches!(action, "web_import_options" | "import_url"), 503)?;
    if action == "search" {
      return Ok(json!({"sources":[],"status":"awaiting_sync"}));
    }
    return Ok(json!({"documents":[],"sources":[],"pages":[],"status":"awaiting_sync"}));
  }
  let key = text(&state["service"], "api_key")?;
  let kb = text(mapping, "id")?;
  if matches!(action, "web_import_options" | "import_url") {
    return web_import(client, key, kb, action, payload).await;
  }
  if action == "sources" {
    let mut url = reqwest::Url::parse("http://internal/datasource")?;
    url.query_pairs_mut().append_pair("kb_id", kb);
    let response = client
      .weknora(
        key,
        &format!("datasource?{}", url.query().unwrap()),
        Method::GET,
        None,
      )
      .await?;
    let fields = [
      "id",
      "name",
      "type",
      "status",
      "last_sync_at",
      "total_items_synced",
    ];
    let sources = array(&response)?
      .iter()
      .filter(|s| s["knowledge_base_id"] == kb)
      .map(|s| {
        Value::Object(
          fields
            .iter()
            .map(|k| (k.to_string(), s[k].clone()))
            .collect(),
        )
      })
      .collect::<Vec<_>>();
    return Ok(json!({"sources":sources,"status":"ready"}));
  }
  let visible = client
    .views(token, &wid)
    .await?
    .into_iter()
    .map(|(view, _)| text(&view, "view_id").map(str::to_owned))
    .collect::<Result<HashSet<_>>>()?;
  let uid = session.uid.to_string();
  if action == "search" {
    return search(
      client,
      key,
      kb,
      &wid,
      &uid,
      &visible,
      text(payload, "query")?,
    )
    .await;
  }
  if matches!(action, "wiki" | "wiki_page") {
    return wiki(
      client,
      key,
      kb,
      &wid,
      &uid,
      &visible,
      info["can_manage"] == true,
      action,
      payload,
    )
    .await;
  }
  if action == "document" {
    let id = uuid(payload, "document_id")?;
    let response = client
      .weknora(key, &format!("knowledge/{id}"), Method::GET, None)
      .await?;
    let entry = &response["data"];
    let mut source =
      entry_source(entry, kb, &wid, &uid, &visible).ok_or_else(|| anyhow!(Rejected(403)))?;
    let mut content = entry["metadata"]["content"].as_str().unwrap_or("");
    if content.starts_with("来源：小马笔记\n\n") {
      content = content.splitn(4, "\n\n").last().unwrap_or("");
    }
    source["content"] = json!(content.chars().take(200000).collect::<String>());
    source["truncated"] = json!(content.chars().count() > 200000);
    return Ok(source);
  }
  require(action == "documents", 404)?;
  let mut documents = Vec::new();
  for entry in client.entries(key, kb).await? {
    if let Some(mut source) = entry_source(&entry, kb, &wid, &uid, &visible) {
      source["id"] = entry["id"].clone();
      source["status"] = json!(entry["parse_status"].as_str().unwrap_or(""));
      documents.push(source);
    }
  }
  Ok(json!({"documents":documents,"status":"ready"}))
}

pub(crate) fn entry_source(
  entry: &Value,
  kb: &str,
  wid: &str,
  uid: &str,
  visible: &HashSet<String>,
) -> Option<Value> {
  if entry["knowledge_base_id"] != kb {
    return None;
  }
  let content = entry["metadata"]["content"].as_str().unwrap_or("");
  let url = if !crate::managed(entry["channel"].as_str().unwrap_or("")) {
    if !matches!(entry["type"].as_str(), Some("file" | "url" | "manual"))
      || content.starts_with("来源：小马笔记\n\n")
    {
      return None;
    }
    String::new()
  } else if content.starts_with("来源：小马笔记\n\n目录：随手记\n\n") {
    if !content.starts_with(&format!(
      "来源：小马笔记\n\n目录：随手记\n\n所属用户：{uid}\n\n"
    )) {
      return None;
    }
    String::new()
  } else {
    crate::source_url(content, wid, visible)?
  };
  Some(json!({"title":entry["title"],"url":url}))
}

async fn search(
  client: &Client,
  key: &str,
  kb: &str,
  wid: &str,
  uid: &str,
  visible: &HashSet<String>,
  query: &str,
) -> Result<Value> {
  let base = client
    .weknora(key, &format!("knowledge-bases/{kb}"), Method::GET, None)
    .await?;
  ensure!(
    base["data"]["embedding_model_id"]
      .as_str()
      .is_some_and(|s| !s.is_empty()),
    "embedding not configured"
  );
  let hits = client
    .weknora(
      key,
      &format!("knowledge-bases/{kb}/hybrid-search"),
      Method::POST,
      Some(&json!({"query_text":query.trim(),"match_count":12,"vector_threshold":0.35})),
    )
    .await?;
  let mut sources = Vec::new();
  let mut seen = HashSet::new();
  let mut budget = 16000;
  for hit in optional_array(&hits["data"])? {
    let Some(kid) = hit["knowledge_id"].as_str() else {
      continue;
    };
    if seen.contains(kid) {
      continue;
    }
    let entry = client
      .weknora(key, &format!("knowledge/{kid}"), Method::GET, None)
      .await?;
    let Some(mut source) = entry_source(&entry["data"], kb, wid, uid, visible) else {
      continue;
    };
    let chunk = hit["content"].as_str().unwrap_or("");
    if chunk.trim().is_empty() {
      continue;
    }
    let chunk = chunk.chars().take(3500.min(budget)).collect::<String>();
    budget -= chunk.chars().count();
    source["content"] = json!(chunk);
    sources.push(source);
    seen.insert(kid.to_owned());
    if sources.len() == 5 || budget == 0 {
      break;
    }
  }
  Ok(json!({"status":if sources.is_empty(){"no_match"}else{"matched"},"sources":sources}))
}

pub(crate) async fn web_import(
  client: &Client,
  key: &str,
  kb: &str,
  action: &str,
  payload: &Value,
) -> Result<Value> {
  let base = client
    .weknora(key, &format!("knowledge-bases/{kb}"), Method::GET, None)
    .await?;
  let mut chunking = base["data"]["chunking_config"]
    .as_object()
    .cloned()
    .unwrap_or_default();
  if action == "web_import_options" {
    return Ok(
      json!({"chunk_size":chunking.get("chunk_size").and_then(Value::as_u64).filter(|n| *n>0).unwrap_or(512),
      "chunk_overlap":chunking.get("chunk_overlap").and_then(Value::as_u64).unwrap_or(80),
      "strategy":chunking.get("strategy").and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or("auto")}),
    );
  }
  let url = payload["url"]
    .as_str()
    .ok_or_else(|| anyhow!(Rejected(400)))?
    .trim();
  require(
    (1..=2048).contains(&url.chars().count())
      && !url.chars().any(|c| c.is_whitespace() || c.is_control()),
    400,
  )?;
  let parsed = reqwest::Url::parse(url).map_err(|_| anyhow!(Rejected(400)))?;
  require(
    matches!(parsed.scheme(), "http" | "https")
      && parsed.host_str().is_some()
      && parsed.username().is_empty()
      && parsed.password().is_none(),
    400,
  )?;
  let size = payload["chunk_size"]
    .as_u64()
    .ok_or_else(|| anyhow!(Rejected(400)))?;
  let overlap = payload["chunk_overlap"]
    .as_u64()
    .ok_or_else(|| anyhow!(Rejected(400)))?;
  require(
    (100..=4000).contains(&size)
      && overlap <= 500
      && overlap < size
      && matches!(
        payload["strategy"].as_str(),
        Some("auto" | "heading" | "heuristic" | "recursive" | "legacy")
      ),
    400,
  )?;
  for field in ["chunk_size", "chunk_overlap", "strategy"] {
    chunking.insert(field.into(), payload[field].clone());
  }
  // WeKnora performs the fetch and its SSRF checks. Never fetch arbitrary URLs in Cloud.
  let entry = client
    .weknora(
      key,
      &format!("knowledge-bases/{kb}/knowledge/url"),
      Method::POST,
      Some(&json!({"url":url,"process_config":{"chunking_config":chunking}})),
    )
    .await?;
  Ok(json!({"document_id":text(&entry["data"],"id")?}))
}

pub(crate) fn wiki_readable(
  page: &Value,
  kb: &str,
  allowed: &HashSet<String>,
  owner: bool,
) -> bool {
  if page["knowledge_base_id"] != kb {
    return false;
  }
  match page["source_refs"].as_array() {
    None => page["source_refs"].is_null() && owner,
    Some(refs) if refs.is_empty() => owner,
    Some(refs) => refs.iter().all(|r| {
      r.as_str()
        .is_some_and(|s| allowed.contains(s.split('|').next().unwrap_or("")))
    }),
  }
}
async fn wiki(
  client: &Client,
  key: &str,
  kb: &str,
  wid: &str,
  uid: &str,
  visible: &HashSet<String>,
  owner: bool,
  action: &str,
  payload: &Value,
) -> Result<Value> {
  let base = client
    .weknora(key, &format!("knowledge-bases/{kb}"), Method::GET, None)
    .await?;
  if base["data"]["indexing_strategy"]["wiki_enabled"] != true {
    require(action != "wiki_page", 404)?;
    return Ok(json!({"pages":[],"status":"ready"}));
  }
  let allowed = client
    .entries(key, kb)
    .await?
    .iter()
    .filter(|e| entry_source(e, kb, wid, uid, visible).is_some())
    .map(|e| text(e, "id").map(str::to_owned))
    .collect::<Result<HashSet<_>>>()?;
  if action == "wiki_page" {
    let slug = payload["slug"]
      .as_str()
      .ok_or_else(|| anyhow!(Rejected(400)))?;
    require(
      (1..=255).contains(&slug.chars().count())
        && !slug.chars().any(|c| c.is_control() || c == '\\')
        && slug.split('/').all(|s| !matches!(s, "" | "." | "..")),
      400,
    )?;
    let mut url = reqwest::Url::parse("http://internal/")?;
    url
      .path_segments_mut()
      .unwrap()
      .extend(["knowledgebase", kb, "wiki", "pages"])
      .extend(slug.split('/'));
    let page = client
      .weknora(key, url.path().trim_start_matches('/'), Method::GET, None)
      .await?;
    require(wiki_readable(&page, kb, &allowed, owner), 403)?;
    let content = page["content"].as_str().unwrap_or("");
    return Ok(
      json!({"title":page["title"],"content":content.chars().take(200000).collect::<String>(),"truncated":content.chars().count()>200000}),
    );
  }
  let mut pages = Vec::new();
  let mut seen = HashSet::new();
  for number in 1.. {
    let response = client
      .weknora(
        key,
        &format!("knowledgebase/{kb}/wiki/pages?page={number}&page_size=100"),
        Method::GET,
        None,
      )
      .await?;
    let batch = optional_array(&response["pages"])?;
    let total = response["total"]
      .as_u64()
      .ok_or_else(|| anyhow!("missing wiki total"))? as usize;
    for page in &batch {
      ensure!(
        seen.insert(text(page, "slug")?.to_owned()),
        "unstable wiki pagination"
      );
      pages.push((*page).clone());
    }
    if pages.len() == total {
      break;
    }
    ensure!(
      !batch.is_empty() && pages.len() < total,
      "incomplete wiki listing"
    );
  }
  pages.retain(|p| wiki_readable(p, kb, &allowed, owner));
  let slugs = pages
    .iter()
    .map(|p| text(p, "slug").map(str::to_owned))
    .collect::<Result<HashSet<_>>>()?;
  let pages = pages.iter().map(|p| json!({"id":p["slug"],"title":p["title"],
    "summary":p["summary"].as_str().unwrap_or(""),"status":p["status"].as_str().unwrap_or(""),
    "out_links":p["out_links"].as_array().into_iter().flatten().filter(|v| v.as_str().is_some_and(|s| slugs.contains(s))).collect::<Vec<_>>()})).collect::<Vec<_>>();
  Ok(json!({"pages":pages,"status":"ready"}))
}
