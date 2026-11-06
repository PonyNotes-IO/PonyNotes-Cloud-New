//! Shared WeKnora integration used by the existing Cloud and Worker processes.
mod notes;
pub mod sync;

use anyhow::{anyhow, ensure, Result};
use reqwest::{Client as HttpClient, Method, StatusCode};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::time::Duration;
use uuid::Uuid;

pub fn enabled() -> bool {
  std::env::var("PONYNOTES_WEKNORA_ENABLED").as_deref() == Ok("true")
}

#[derive(Clone)]
pub struct Client {
  http: HttpClient,
  base: String,
  cloud: String,
}

impl Client {
  pub fn from_env() -> Result<Self> {
    Self::new(
      std::env::var("PONYNOTES_WEKNORA_URL")
        .unwrap_or_else(|_| "http://ponynotes-weknora-api:8080/api/v1/".into()),
      std::env::var("PONYNOTES_WEKNORA_CLOUD_URL").unwrap_or_else(|_| "http://nginx".into()),
    )
  }

  pub fn new(base: String, cloud: String) -> Result<Self> {
    Ok(Self {
      http: HttpClient::builder()
        .timeout(Duration::from_secs(90))
        .redirect(reqwest::redirect::Policy::none())
        .build()?,
      base: base.trim_end_matches('/').to_owned() + "/",
      cloud: cloud.trim_end_matches('/').to_owned(),
    })
  }

  async fn request(
    &self,
    url: &str,
    method: Method,
    auth: Option<(&str, &str)>,
    payload: Option<&Value>,
  ) -> Result<Value> {
    let mut request = self.http.request(method, url);
    if let Some((header, value)) = auth {
      request = request.header(header, value);
    }
    if let Some(payload) = payload {
      request = request.json(payload);
    }
    // Never propagate URLs, HTTP bodies or credentials through errors or tracing.
    let response = request
      .send()
      .await
      .map_err(|_| anyhow!("knowledge upstream unavailable"))?;
    let status = response.status();
    if !status.is_success() {
      return Err(anyhow!(UpstreamStatus(status)));
    }
    let result: Value = response
      .json()
      .await
      .map_err(|_| anyhow!("invalid upstream response"))?;
    ensure!(result.is_object(), "invalid upstream response");
    ensure!(
      result.get("success") != Some(&json!(false)) && result.get("code").is_none_or(|v| v == 0),
      "upstream rejected request"
    );
    Ok(result)
  }

  pub(crate) async fn weknora(
    &self,
    key: &str,
    path: &str,
    method: Method,
    payload: Option<&Value>,
  ) -> Result<Value> {
    self
      .request(
        &(self.base.clone() + path),
        method,
        Some(("X-API-Key", key)),
        payload,
      )
      .await
  }

  async fn bearer(&self, token: &str, path: &str, payload: &Value) -> Result<Value> {
    self
      .request(
        &(self.base.clone() + path),
        Method::POST,
        Some(("Authorization", &format!("Bearer {token}"))),
        Some(payload),
      )
      .await
  }

  async fn cloud(&self, token: &str, path: &str) -> Result<Value> {
    let result = self
      .request(
        &format!("{}/api/{path}", self.cloud),
        Method::GET,
        Some(("Authorization", &format!("Bearer {token}"))),
        None,
      )
      .await?;
    result
      .get("data")
      .cloned()
      .ok_or_else(|| anyhow!("missing cloud response data"))
  }

  async fn views(&self, token: &str, wid: &str) -> Result<Vec<(Value, String)>> {
    let mut pending = vec![(wid.to_owned(), String::new())];
    let mut visited = HashSet::new();
    let mut views = Vec::new();
    while let Some((oid, parent)) = pending.pop() {
      ensure!(visited.insert(oid.clone()), "cyclic folder");
      let node = self
        .cloud(
          token,
          &format!("workspace/{wid}/folder?depth=1&root_view_id={oid}"),
        )
        .await?;
      ensure!(text(&node, "view_id")? == oid, "folder root mismatch");
      let path = format!(
        "{parent}{}{}",
        if parent.is_empty() { "" } else { " / " },
        text(&node, "name")?
      );
      for child in array(&node["children"])? {
        let id = text(child, "view_id")?;
        Uuid::parse_str(id)?;
        pending.push((id.to_owned(), path.clone()));
      }
      views.push((node, path));
    }
    Ok(views)
  }

  async fn entries(&self, key: &str, kb: &str) -> Result<Vec<Value>> {
    let mut entries = Vec::new();
    let mut seen = HashSet::new();
    for page in 1.. {
      let result = self
        .weknora(
          key,
          &format!("knowledge-bases/{kb}/knowledge?page={page}&page_size=100"),
          Method::GET,
          None,
        )
        .await?;
      let batch = optional_array(&result["data"])?;
      let total = result["total"]
        .as_u64()
        .ok_or_else(|| anyhow!("missing knowledge total"))? as usize;
      for entry in &batch {
        ensure!(
          seen.insert(text(entry, "id")?.to_owned()),
          "unstable knowledge pagination"
        );
        entries.push((*entry).clone());
      }
      if entries.len() == total {
        return Ok(entries);
      }
      ensure!(
        !batch.is_empty() && entries.len() < total,
        "incomplete knowledge pagination"
      );
    }
    unreachable!()
  }
}

#[derive(Debug)]
struct UpstreamStatus(StatusCode);
impl std::fmt::Display for UpstreamStatus {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(f, "upstream status {}", self.0.as_u16())
  }
}
impl std::error::Error for UpstreamStatus {}
fn not_found(error: &anyhow::Error) -> bool {
  error
    .downcast_ref::<UpstreamStatus>()
    .is_some_and(|e| e.0 == StatusCode::NOT_FOUND)
}

fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
  value
    .get(key)
    .and_then(Value::as_str)
    .ok_or_else(|| anyhow!("missing string field {key}"))
}
fn array(value: &Value) -> Result<&Vec<Value>> {
  value.as_array().ok_or_else(|| anyhow!("missing array"))
}
fn optional_array(value: &Value) -> Result<Vec<&Value>> {
  if value.is_null() {
    Ok(Vec::new())
  } else {
    Ok(array(value)?.iter().collect())
  }
}
fn managed(channel: &str) -> bool {
  channel.len() == 43
    && channel.starts_with("pn:")
    && channel[3..]
      .bytes()
      .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

/// The caller supplies the authenticated identity and only that user's DB mapping.
pub async fn search(
  client: &Client,
  account: Option<&Value>,
  uid: Uuid,
  token: &str,
  wid: Uuid,
  query: &str,
) -> Result<Value> {
  ensure!(
    (1..=2000).contains(&query.trim().chars().count()),
    "invalid search query"
  );
  let session = client
    .request(
      &format!("{}/gotrue/user", client.cloud),
      Method::GET,
      Some(("Authorization", &format!("Bearer {token}"))),
      None,
    )
    .await?;
  ensure!(
    text(&session, "id")? == uid.to_string(),
    "login identity mismatch"
  );
  let wid = wid.to_string();
  let workspaces = client.cloud(token, "workspace").await?;
  ensure!(
    array(&workspaces)?.iter().any(|w| w["workspace_id"] == wid),
    "workspace access denied"
  );
  let visible: HashSet<String> = client
    .views(token, &wid)
    .await?
    .iter()
    .map(|(node, _)| text(node, "view_id").map(str::to_owned))
    .collect::<Result<_>>()?;
  let Some(account) = account else {
    return Ok(json!({"sources":[], "status":"awaiting_sync"}));
  };
  let mapping = &account["workspaces"][&wid];
  if mapping.is_null() {
    return Ok(json!({"sources":[], "status":"awaiting_sync"}));
  }
  let key = text(account, "api_key")?;
  let kb = text(mapping, "id")?;
  let knowledge_base = client
    .weknora(key, &format!("knowledge-bases/{kb}"), Method::GET, None)
    .await?;
  ensure!(
    knowledge_base["data"]["embedding_model_id"]
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
    let response = client
      .weknora(key, &format!("knowledge/{kid}"), Method::GET, None)
      .await?;
    let entry = &response["data"];
    if entry["knowledge_base_id"] != kb || !managed(entry["channel"].as_str().unwrap_or("")) {
      continue;
    }
    let content = entry["metadata"]["content"].as_str().unwrap_or("");
    let Some(url) = source_url(content, &wid, &visible) else {
      continue;
    };
    let chunk = hit["content"].as_str().unwrap_or("");
    if chunk.trim().is_empty() {
      continue;
    }
    let chunk: String = chunk.chars().take(3500.min(budget)).collect();
    budget -= chunk.chars().count();
    sources.push(json!({"title":text(entry,"title")?,"url":url,"content":chunk}));
    seen.insert(kid.to_owned());
    if sources.len() == 5 || budget == 0 {
      break;
    }
  }
  Ok(json!({"status":if sources.is_empty(){"no_match"}else{"matched"},"sources":sources}))
}

fn source_url(content: &str, wid: &str, visible: &HashSet<String>) -> Option<String> {
  if let Some(start) = content.find("ponynotes://open?viewId=") {
    let link = content[start..].split([')', '\n', ' ', '\r']).next()?;
    let ids = link.strip_prefix("ponynotes://open?viewId=")?;
    let (view, workspace) = ids.split_once("&workspaceId=")?;
    if workspace == wid && Uuid::parse_str(view).is_ok() && visible.contains(view) {
      return Some(link.to_owned());
    }
    None
  } else if content.starts_with("来源：小马笔记\n\n目录：随手记\n") {
    Some(String::new())
  } else {
    None
  }
}

#[cfg(test)]
mod tests;
