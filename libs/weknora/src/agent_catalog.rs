//! Agent identities come from the caller's WeKnora tenant and verified shares.
use crate::{array, text, Client};
use anyhow::{ensure, Result};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use uuid::Uuid;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Selection {
  pub id: String,
  #[serde(default)]
  pub source_tenant_id: Option<u64>,
}

impl Selection {
  pub fn valid(&self) -> bool {
    (Uuid::parse_str(&self.id).is_ok()
      || matches!(
        self.id.as_str(),
        "builtin-quick-answer"
          | "builtin-smart-reasoning"
          | "builtin-wiki-researcher"
          | "builtin-data-analyst"
          | "builtin-deep-researcher"
          | "builtin-knowledge-graph-expert"
          | "builtin-document-assistant"
      ))
      && self.source_tenant_id != Some(0)
  }
}

/// Explicit references are handled separately by Cloud. Implicit retrieval
/// follows the selected agent's KB scope within Cloud's authorized documents.
pub fn permits_document(config: &Value, source: &Value, managed_base: Option<&str>) -> bool {
  match config["kb_selection_mode"].as_str().unwrap_or("none") {
    "all" => true,
    "selected" => source["base_id"]
      .as_str()
      .or(managed_base)
      .is_some_and(|base| {
        config["knowledge_bases"]
          .as_array()
          .is_some_and(|bases| bases.iter().any(|id| id == base))
      }),
    _ => false,
  }
}

pub fn metadata(agent: &Value, source: Option<u64>) -> Result<Value> {
  let selection = Selection {
    id: text(agent, "id")?.to_owned(),
    source_tenant_id: source,
  };
  ensure!(selection.valid(), "invalid agent identity");
  let cfg = &agent["config"];
  ensure!(
    matches!(
      cfg["agent_mode"].as_str(),
      Some("quick-answer" | "smart-reasoning")
    ),
    "invalid agent mode"
  );
  let name = match selection.id.as_str() {
    "builtin-quick-answer" => "快速问答",
    "builtin-smart-reasoning" => "智能推理",
    "builtin-wiki-researcher" => "维基问答",
    "builtin-data-analyst" => "数据分析师",
    _ => text(agent, "name")?,
  };
  Ok(json!({"id":selection.id,"source_tenant_id":source,
    "name":name,"description":agent["description"].as_str().unwrap_or(""),
    "mode":cfg["agent_mode"],"group":if source.is_some(){"shared"}else if agent["is_builtin"]==true{"builtin"}else{"private"},
    "knowledge_scope":cfg["kb_selection_mode"].as_str().unwrap_or("none"),
    "multi_turn":cfg["multi_turn_enabled"]==true,"web_search":cfg["web_search_enabled"]==true,
    "image_upload":cfg["image_upload_enabled"]==true}))
}

impl Client {
  pub async fn agent_catalog(&self, token: &str, runtime_ids: &[String]) -> Result<Vec<Value>> {
    let own = self
      .agent_request(token, "agents", Method::GET, None)
      .await?;
    let shared = self
      .agent_request(token, "shared-agents", Method::GET, None)
      .await?;
    let mut result = Vec::new();
    let mut seen = HashSet::new();
    for agent in array(&own["data"])? {
      let id = text(agent, "id")?;
      if runtime_ids.iter().any(|runtime| runtime == id)
        || own["disabled_own_agent_ids"]
          .as_array()
          .is_some_and(|ids| ids.iter().any(|v| v == id))
        || agent["description"]
          .as_str()
          .unwrap_or("")
          .starts_with("PonyNotes conversation:")
      {
        continue;
      }
      if seen.insert((id.to_owned(), None)) {
        result.push(metadata(agent, None)?);
      }
    }
    for item in array(&shared["data"])? {
      if item["disabled_by_me"] == true {
        continue;
      }
      let source = item["source_tenant_id"].as_u64();
      ensure!(
        source.is_some_and(|id| id > 0),
        "invalid shared agent source"
      );
      let agent = &item["agent"];
      if seen.insert((text(agent, "id")?.to_owned(), source)) {
        result.push(metadata(agent, source)?);
      }
    }
    Ok(result)
  }

  /// Resolve shared configuration only in WeKnora at execution time: its list
  /// intentionally withholds the owner's prompts and cannot be copied faithfully.
  pub async fn selected_agent(&self, token: &str, selection: &Selection) -> Result<Value> {
    ensure!(selection.valid(), "invalid agent selection");
    if let Some(source) = selection.source_tenant_id {
      let shared = self
        .agent_request(token, "shared-agents", Method::GET, None)
        .await?;
      let item = array(&shared["data"])?.iter().find(|item| {
        item["agent"]["id"] == selection.id
          && item["source_tenant_id"] == source
          && item["disabled_by_me"] != true
      });
      ensure!(item.is_some(), "shared agent unavailable");
      return Ok(item.unwrap()["agent"].clone());
    }
    let own = self
      .agent_request(token, "agents", Method::GET, None)
      .await?;
    ensure!(
      !own["disabled_own_agent_ids"]
        .as_array()
        .is_some_and(|ids| ids.iter().any(|id| id == &selection.id)),
      "agent disabled"
    );
    ensure!(
      array(&own["data"])?
        .iter()
        .any(|agent| agent["id"] == selection.id
          && !agent["description"]
            .as_str()
            .unwrap_or("")
            .starts_with("PonyNotes conversation:")),
      "agent unavailable"
    );
    let agent = self
      .agent_request(
        token,
        &format!("agents/{}", selection.id),
        Method::GET,
        None,
      )
      .await?;
    Ok(agent["data"].clone())
  }
}
