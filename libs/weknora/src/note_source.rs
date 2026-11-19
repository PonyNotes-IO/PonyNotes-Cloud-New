use crate::{
  array, data_source,
  library::{load, require, uuid, Session},
  not_found, text, Client,
};
use anyhow::{anyhow, ensure, Result};
use reqwest::Method;
use serde_json::{json, Value};
use sqlx::PgPool;
use std::time::Duration;
use uuid::Uuid;

pub const WORKSPACE_SYNC_SNAPSHOT: &str = "ponynotes_feishu_snapshot";

const STAGING_NAME: &str = "PonyNotes workspace sync (internal)";

fn staging_id(workspace: Uuid, user: Uuid) -> Uuid {
  Uuid::new_v5(&workspace, format!("workspace-note-sync:{user}").as_bytes())
}

pub struct WorkspaceNoteSources {
  client: Client,
  key: String,
  staging: String,
}

impl WorkspaceNoteSources {
  pub async fn open(
    pool: &PgPool,
    client: &Client,
    session: &Session,
    payload: &Value,
  ) -> Result<Self> {
    let workspace = uuid(payload, "workspace_id")?;
    session.can_write(&workspace.to_string())?;
    require(payload.get("base_id").is_none(), 400)?;
    let mut connection = pool.acquire().await?;
    let (state, _) = load(&mut connection, false).await?;
    Ok(Self {
      client: client.clone(),
      key: text(&state["service"], "api_key")?.to_owned(),
      staging: staging_id(workspace, session.uid).to_string(),
    })
  }

  async fn staging_exists(&self) -> Result<bool> {
    match self
      .client
      .weknora(
        &self.key,
        &format!("knowledge-bases/{}", self.staging),
        Method::GET,
        None,
      )
      .await
    {
      Ok(base) => {
        let data = &base["data"];
        let indexing = &data["indexing_strategy"];
        ensure!(
          data["id"] == self.staging
            && data["name"] == STAGING_NAME
            && [
              "vector_enabled",
              "keyword_enabled",
              "wiki_enabled",
              "graph_enabled"
            ]
            .iter()
            .all(|&flag| indexing[flag] == false),
          "invalid sync staging configuration"
        );
        Ok(true)
      },
      Err(err) if not_found(&err) => Ok(false),
      Err(err) => Err(err),
    }
  }

  pub async fn execute(&self, payload: &Value) -> Result<Value> {
    let mut payload = payload.clone();
    match payload["operation"].as_str() {
      Some("types") => {
        let result = data_source::execute(&self.client, &self.key, &self.staging, &payload).await?;
        let mut types = array(&result["types"])?.clone();
        types.retain(|kind| kind["type"] == "feishu");
        for kind in &mut types {
          if let Some(fields) = kind["fields"].as_array_mut() {
            fields.retain(|field| field["key"] != "base_url");
          }
        }
        return Ok(json!({"types": types}));
      },
      Some("create" | "test") => require(payload["type"] == "feishu", 400)?,
      Some("save") => {
        payload["sync_schedule"] = json!("");
        payload["sync_mode"] = json!("incremental");
        payload["conflict_strategy"] = json!("overwrite");
        payload["sync_deletions"] = json!(false);
      },
      Some("list" | "resources" | "credentials" | "validate" | "delete") => (),
      _ => require(false, 400)?,
    }
    require(payload["credentials"].get("base_url").is_none(), 400)?;
    match payload["operation"].as_str() {
      Some("list") if !self.staging_exists().await? => return Ok(json!({"sources": []})),
      Some("create") if !self.staging_exists().await? => {
        // ponytail: the existing connector requires a KB internally. Keep its
        // files/cursor in an unlisted per-user staging KB with indexing off.
        let created = self
          .client
          .weknora(
            &self.key,
            "knowledge-bases",
            Method::POST,
            Some(&json!({
              "id": self.staging, "name": STAGING_NAME, "type": "document",
              "indexing_strategy": {"vector_enabled": false, "keyword_enabled": false,
                "wiki_enabled": false, "graph_enabled": false},
            })),
          )
          .await;
        if !self.staging_exists().await? {
          created?;
          anyhow::bail!("missing sync staging area");
        }
      },
      _ => (),
    }
    let mut result = data_source::execute(&self.client, &self.key, &self.staging, &payload).await?;
    if payload["operation"] == "list" {
      if let Some(sources) = result["sources"].as_array_mut() {
        sources.retain(|source| source["type"] == "feishu");
      }
    }
    Ok(result)
  }

  pub async fn source(&self, payload: &Value) -> Result<FeishuNoteSource> {
    require(self.staging_exists().await?, 404)?;
    FeishuNoteSource::open(
      self.client.clone(),
      self.key.clone(),
      self.staging.clone(),
      uuid(payload, "source_id")?,
    )
    .await
  }
}

pub struct FeishuNoteSource {
  client: Client,
  key: String,
  kb: String,
  source_id: Uuid,
  resources: Vec<Value>,
}

pub struct FeishuNote {
  pub external_id: String,
  pub title: String,
  knowledge_id: Uuid,
}

impl FeishuNoteSource {
  pub(crate) async fn open(
    client: Client,
    key: String,
    kb: String,
    source_id: Uuid,
  ) -> Result<Self> {
    let source = client
      .weknora(&key, &format!("datasource/{source_id}"), Method::GET, None)
      .await?;
    require(source["knowledge_base_id"] == kb, 403)?;
    require(source["type"] == "feishu", 400)?;
    let resources = array(&source["config"]["resource_ids"])?.clone();
    require(!resources.is_empty(), 400)?;
    Ok(Self {
      client,
      key,
      kb,
      source_id,
      resources,
    })
  }

  async fn call(&self, operation: &str) -> Result<Value> {
    data_source::execute(
      &self.client,
      &self.key,
      &self.kb,
      &json!({"operation": operation, "source_id": self.source_id}),
    )
    .await
  }

  pub async fn refresh(&self) -> Result<()> {
    let existing = self.call("logs").await?;
    let mut log = match existing["logs"].get(0) {
      Some(log) if log["status"] == "running" => log.clone(),
      _ => self.call("sync").await?["log"].clone(),
    };
    let id = Uuid::parse_str(text(&log, "id")?)?;
    while log["status"] == "running" {
      tokio::time::sleep(Duration::from_secs(3)).await;
      log = self
        .client
        .weknora(
          &self.key,
          &format!("datasource/logs/{id}"),
          Method::GET,
          None,
        )
        .await?;
    }
    ensure!(
      log["status"] == "success",
      "feishu refresh did not finish successfully"
    );
    self.check_scope().await
  }

  pub async fn check_scope(&self) -> Result<()> {
    let current = self
      .client
      .weknora(
        &self.key,
        &format!("datasource/{}", self.source_id),
        Method::GET,
        None,
      )
      .await?;
    require(
      current["knowledge_base_id"] == self.kb && current["type"] == "feishu",
      403,
    )?;
    let resources = array(&current["config"]["resource_ids"])?;
    require(resources == &self.resources, 409)
  }

  pub async fn notes(&self) -> Result<Vec<FeishuNote>> {
    self
      .client
      .entries(&self.key, &self.kb)
      .await?
      .iter()
      .filter(|entry| self.matches(entry))
      .map(|entry| {
        let external_id = text(&entry["metadata"], "external_id")?;
        let title = entry["title"]
          .as_str()
          .filter(|title| !title.is_empty())
          .or_else(|| {
            entry["file_name"]
              .as_str()
              .and_then(|name| name.strip_suffix(".md"))
          })
          .ok_or_else(|| anyhow!("missing note title"))?;
        Ok(FeishuNote {
          external_id: external_id.to_owned(),
          title: title.to_owned(),
          knowledge_id: Uuid::parse_str(text(entry, "id")?)?,
        })
      })
      .collect()
  }

  fn matches(&self, entry: &Value) -> bool {
    let meta = &entry["metadata"];
    entry["knowledge_base_id"] == self.kb
      && meta["datasource_id"] == self.source_id.to_string()
      && meta["obj_type"] == "docx"
      && self.resources.contains(&meta["source_resource_id"])
      && meta["node_token"] == meta["external_id"]
      && meta["node_token"].as_str().is_some_and(|id| {
        !id.is_empty() && id.len() <= 128 && id.bytes().all(|b| b.is_ascii_alphanumeric())
      })
      && entry["file_name"]
        .as_str()
        .is_some_and(|name| name.ends_with(".md"))
  }

  pub async fn markdown(&self, note: &FeishuNote) -> Result<String> {
    // Read the original Markdown file from WeKnora, not its search chunks.
    let mut response = self
      .client
      .http
      .get(format!(
        "{}knowledge/{}/download",
        self.client.base, note.knowledge_id
      ))
      .header("X-API-Key", &self.key)
      .send()
      .await
      .map_err(|_| anyhow!("note download unavailable"))?;
    ensure!(response.status().is_success(), "note download rejected");
    const MAX_BYTES: usize = 24 * 1024 * 1024;
    let mut bytes = Vec::new();
    while let Some(chunk) = response
      .chunk()
      .await
      .map_err(|_| anyhow!("incomplete note download"))?
    {
      ensure!(
        bytes.len() + chunk.len() <= MAX_BYTES,
        "note exceeds download limit"
      );
      bytes.extend_from_slice(&chunk);
    }
    Ok(String::from_utf8(bytes)?)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn workspace_sync_staging_is_scoped_by_workspace_and_user() {
    let workspace = Uuid::new_v4();
    let user = Uuid::new_v4();
    let id = staging_id(workspace, user);
    assert_eq!(id, staging_id(workspace, user));
    assert_ne!(id, staging_id(workspace, Uuid::new_v4()));
    assert_ne!(id, staging_id(Uuid::new_v4(), user));
  }

  #[test]
  fn only_selected_wiki_documents_are_eligible() {
    let source_id = Uuid::new_v4();
    let source = FeishuNoteSource {
      client: Client::new("http://localhost".into(), "http://localhost".into()).unwrap(),
      key: String::new(),
      kb: "base".into(),
      source_id,
      resources: vec![json!("selected")],
    };
    let doc = json!({"knowledge_base_id":"base", "file_name":"A.md", "metadata": {
      "datasource_id":source_id, "obj_type":"docx", "node_token":"node123", "external_id":"node123", "source_resource_id":"selected"}});
    assert!(source.matches(&doc));
    for (field, value) in [
      ("datasource_id", json!(Uuid::new_v4())),
      ("source_resource_id", json!("old")),
      ("external_id", json!("node123#image#1")),
      ("obj_type", json!("sheet")),
    ] {
      let mut other = doc.clone();
      other["metadata"][field] = value;
      assert!(!source.matches(&other));
    }
  }
}
