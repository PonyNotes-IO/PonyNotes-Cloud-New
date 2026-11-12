//! Shared WeKnora integration used by the existing Cloud and Worker processes.
pub mod agent;
pub mod embedding;
pub mod library;
mod notes;
pub mod references;
pub mod sync;
mod web_import;

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
  ollama: String,
}

impl Client {
  pub async fn agent_file(&self, token: &str, path: &str) -> Result<reqwest::Response> {
    let response = self
      .http
      .get(self.base.clone() + path)
      .bearer_auth(token)
      .timeout(Duration::from_secs(600))
      .send()
      .await
      .map_err(|_| anyhow!("knowledge upstream unavailable"))?;
    ensure!(response.status().is_success(), "artifact unavailable");
    Ok(response)
  }

  /// Authenticated JSON calls stay on the configured internal WeKnora origin.
  pub async fn agent_request(
    &self,
    token: &str,
    path: &str,
    method: Method,
    payload: Option<&Value>,
  ) -> Result<Value> {
    self
      .request(
        &(self.base.clone() + path),
        method,
        Some(("Authorization", &format!("Bearer {token}"))),
        payload,
      )
      .await
  }

  pub async fn agent_stream(
    &self,
    token: &str,
    path: &str,
    method: Method,
    payload: Option<&Value>,
  ) -> Result<reqwest::Response> {
    let mut request = self
      .http
      .request(method, self.base.clone() + path)
      .bearer_auth(token)
      .timeout(Duration::from_secs(600));
    if let Some(payload) = payload {
      request = request.json(payload);
    }
    let response = request
      .send()
      .await
      .map_err(|_| anyhow!("knowledge upstream unavailable"))?;
    ensure!(response.status().is_success(), "agent request rejected");
    ensure!(
      response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|s| s.starts_with("text/event-stream")),
      "invalid agent stream"
    );
    Ok(response)
  }

  pub fn model_base_url(&self) -> String {
    std::env::var("PONYNOTES_WEKNORA_MODEL_BASE_URL")
      .unwrap_or_else(|_| format!("{}/api/ai/agent", self.cloud))
  }
  pub fn from_env() -> Result<Self> {
    let mut client = Self::new(
      std::env::var("PONYNOTES_WEKNORA_URL")
        .unwrap_or_else(|_| "http://ponynotes-weknora-api:8080/api/v1/".into()),
      std::env::var("PONYNOTES_WEKNORA_CLOUD_URL").unwrap_or_else(|_| "http://nginx".into()),
    )?;
    client.ollama = std::env::var("PONYNOTES_WEKNORA_OLLAMA_URL")
      .unwrap_or_else(|_| "http://host.docker.internal:11434".into());
    Ok(client)
  }

  pub fn new(base: String, cloud: String) -> Result<Self> {
    Ok(Self {
      http: HttpClient::builder()
        .timeout(Duration::from_secs(90))
        .redirect(reqwest::redirect::Policy::none())
        .build()?,
      base: base.trim_end_matches('/').to_owned() + "/",
      cloud: cloud.trim_end_matches('/').to_owned(),
      ollama: "http://host.docker.internal:11434".into(),
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
    if status == StatusCode::NO_CONTENT {
      return Ok(Value::Null);
    }
    let result: Value = response
      .json()
      .await
      .map_err(|_| anyhow!("invalid upstream response"))?;
    ensure!(
      result.is_object() || result.is_array(),
      "invalid upstream response"
    );
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
pub struct UpstreamStatus(pub StatusCode);
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
