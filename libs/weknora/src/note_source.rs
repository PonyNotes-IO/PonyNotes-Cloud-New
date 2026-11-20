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

// Keep the stored key for pages already imported by the Feishu MVP.
pub const WORKSPACE_SYNC_SNAPSHOT: &str = "ponynotes_feishu_snapshot";

const STAGING_NAME: &str = "PonyNotes workspace sync (internal)";

fn supported(kind: &Value) -> bool {
  matches!(
    kind.as_str(),
    Some("feishu" | "notion" | "yuque" | "gitlab")
  )
}

fn gitlab_scope(payload: &mut Value) -> Result<()> {
  let projects = array(&payload["settings"]["projects"])?;
  // ponytail: one immutable project/ref/directory scope per workspace connection.
  // A new scope needs a new connector so its incremental cursor starts cleanly.
  require(projects.len() == 1, 400)?;
  let project = text(&projects[0], "project_id")?.trim().to_owned();
  require(
    project.len() <= 512
      && (project.parse::<u64>().is_ok_and(|id| id > 0) || project.contains('/'))
      && project.split('/').all(|part| {
        !part.is_empty()
          && part != "."
          && part != ".."
          && !part
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || "\\:?#%".contains(c))
      }),
    400,
  )?;
  let branch = text(&projects[0], "ref")?.trim().to_owned();
  require(
    branch.len() <= 255 && !branch.chars().any(char::is_control),
    400,
  )?;
  let paths = array(&projects[0]["paths"])?;
  require(paths.len() <= 50, 400)?;
  let mut roots = Vec::new();
  for path in paths {
    let path = path
      .as_str()
      .ok_or_else(|| anyhow!(crate::library::Rejected(400)))?;
    let path = path.trim().trim_matches('/');
    require(
      path.len() <= 2048
        && !path.chars().any(|c| c.is_control() || c == '\\')
        && (path.is_empty() || path.split('/').all(|part| !matches!(part, "" | "." | ".."))),
      400,
    )?;
    roots.push(json!(path));
  }
  let base = text(&payload["credentials"], "base_url")?
    .trim()
    .trim_end_matches('/');
  let url = reqwest::Url::parse(base).map_err(|_| anyhow!(crate::library::Rejected(400)))?;
  require(
    matches!(url.scheme(), "http" | "https")
      && url.host_str().is_some()
      && url.username().is_empty()
      && url.password().is_none()
      && url.query().is_none()
      && url.fragment().is_none(),
    400,
  )?;
  let base = base.to_owned();
  payload["credentials"]["base_url"] = json!(base);
  payload["settings"] = json!({"base_url":base, "projects":[{
    "project_id":project, "ref":branch, "paths":roots
  }]});
  payload["resource_ids"] = json!([project]);
  Ok(())
}

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
    let gitlab_setup = matches!(payload["operation"].as_str(), Some("create" | "test"))
      && payload["type"] == "gitlab";
    match payload["operation"].as_str() {
      Some("types") => {
        let result = data_source::execute(&self.client, &self.key, &self.staging, &payload).await?;
        let mut types = array(&result["types"])?.clone();
        types.retain(|kind| supported(&kind["type"]));
        for kind in &mut types {
          if kind["type"] != "gitlab" {
            if let Some(fields) = kind["fields"].as_array_mut() {
              fields.retain(|field| field["key"] != "base_url");
            }
          }
        }
        return Ok(json!({"types": types}));
      },
      Some("create" | "test") => {
        require(supported(&payload["type"]), 400)?;
        if gitlab_setup {
          gitlab_scope(&mut payload).map_err(|_| anyhow!(crate::library::Rejected(400)))?;
        }
      },
      Some("save") => {
        payload["sync_schedule"] = json!("");
        payload["sync_mode"] = json!("incremental");
        payload["conflict_strategy"] = json!("overwrite");
        payload["sync_deletions"] = json!(false);
      },
      Some("list" | "resources" | "credentials" | "validate" | "delete") => (),
      _ => require(false, 400)?,
    }
    if !gitlab_setup {
      require(payload["settings"].get("base_url").is_none(), 400)?;
      if let Some(base_url) = payload["credentials"].get("base_url") {
        require(payload["operation"] == "credentials", 400)?;
        let source = self.source(&payload).await?;
        require(source.kind == "gitlab" && base_url == &source.base_url, 400)?;
      }
    }
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
        sources.retain(|source| supported(&source["type"]));
      }
    }
    Ok(result)
  }

  pub async fn source(&self, payload: &Value) -> Result<WorkspaceNoteSource> {
    require(self.staging_exists().await?, 404)?;
    WorkspaceNoteSource::open(
      self.client.clone(),
      self.key.clone(),
      self.staging.clone(),
      uuid(payload, "source_id")?,
    )
    .await
  }
}

pub struct WorkspaceNoteSource {
  client: Client,
  key: String,
  kb: String,
  source_id: Uuid,
  kind: String,
  resources: Vec<Value>,
  projects: Value,
  base_url: Value,
}

pub struct WorkspaceNote {
  pub external_id: String,
  pub title: String,
  pub source_link: Option<String>,
  knowledge_id: Uuid,
  plain_text: bool,
}

impl WorkspaceNoteSource {
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
    require(supported(&source["type"]), 400)?;
    let kind = text(&source, "type")?.to_owned();
    let resources = array(&source["config"]["resource_ids"])?.clone();
    require(!resources.is_empty(), 400)?;
    let projects = source["config"]["settings"]["projects"].clone();
    let base_url = source["config"]["settings"]["base_url"].clone();
    if kind == "gitlab" {
      let mut scope =
        json!({"credentials":{"base_url":base_url}, "settings":{"projects":projects}});
      gitlab_scope(&mut scope)?;
      require(
        scope["resource_ids"] == json!(resources) && scope["settings"]["projects"] == projects,
        400,
      )?;
    }
    Ok(Self {
      client,
      key,
      kb,
      source_id,
      kind,
      resources,
      projects,
      base_url,
    })
  }

  pub fn page_namespace(&self, workspace: Uuid, uid: i64) -> Uuid {
    let kind = if self.kind == "feishu" {
      "feishu-wiki"
    } else {
      &self.kind
    };
    let source_id = self.source_id;
    Uuid::new_v5(&workspace, format!("{kind}:{uid}:{source_id}").as_bytes())
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
      "connector refresh did not finish successfully"
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
      current["knowledge_base_id"] == self.kb && current["type"] == self.kind,
      403,
    )?;
    let resources = array(&current["config"]["resource_ids"])?;
    require(resources == &self.resources, 409)?;
    require(
      self.kind != "gitlab"
        || (current["config"]["settings"]["projects"] == self.projects
          && current["config"]["settings"]["base_url"] == self.base_url),
      409,
    )
  }

  pub async fn notes(&self) -> Result<Vec<WorkspaceNote>> {
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
        Ok(WorkspaceNote {
          external_id: external_id.to_owned(),
          title: title.to_owned(),
          source_link: match self.kind.as_str() {
            "feishu" => Some(format!(
              "[在飞书打开](https://feishu.cn/wiki/{external_id})"
            )),
            "notion" => Some(format!(
              "[在 Notion 打开](https://www.notion.so/{external_id})"
            )),
            _ => None,
          },
          knowledge_id: Uuid::parse_str(text(entry, "id")?)?,
          plain_text: self.kind == "gitlab"
            && entry["file_name"]
              .as_str()
              .is_some_and(|name| name.to_ascii_lowercase().ends_with(".txt")),
        })
      })
      .collect()
  }

  fn matches(&self, entry: &Value) -> bool {
    let meta = &entry["metadata"];
    if entry["knowledge_base_id"] != self.kb
      || meta["datasource_id"] != self.source_id.to_string()
      || !entry["file_name"].as_str().is_some_and(|name| {
        if self.kind == "gitlab" {
          [".md", ".markdown", ".txt"]
            .iter()
            .any(|ext| name.to_ascii_lowercase().ends_with(ext))
        } else {
          name.ends_with(".md")
        }
      })
    {
      return false;
    }
    let Some(id) = meta["external_id"].as_str() else {
      return false;
    };
    match self.kind.as_str() {
      "feishu" => {
        meta["obj_type"] == "docx"
          && self.resources.contains(&meta["source_resource_id"])
          && meta["node_token"] == id
          && !id.is_empty()
          && id.len() <= 128
          && id.bytes().all(|b| b.is_ascii_alphanumeric())
      },
      // ponytail: Notion Markdown rows have no source_resource_id. Copy only
      // explicitly selected pages, never inferred descendants or database exports.
      "notion" => {
        meta["object_type"] == "page"
          && Uuid::parse_str(id).is_ok()
          && self.resources.contains(&meta["external_id"])
      },
      "yuque" => {
        meta["doc_id"] == id
          && id.parse::<u64>().is_ok_and(|id| id > 0)
          && meta["book_id"] == meta["source_resource_id"]
          && self.resources.contains(&meta["source_resource_id"])
      },
      "gitlab" => {
        let selection = &self.projects[0];
        let Some(project) = selection["project_id"].as_str() else {
          return false;
        };
        let Some(path) = meta["gitlab_path"].as_str() else {
          return false;
        };
        let Some(branch) = meta["gitlab_ref"].as_str() else {
          return false;
        };
        let Some(project_id) = meta["gitlab_project_id"].as_str() else {
          return false;
        };
        let base = self.base_url.as_str().unwrap_or("").trim_end_matches('/');
        let canonical = if base.ends_with("/api/v4") {
          base.to_owned()
        } else {
          format!("{base}/api/v4")
        };
        let web_base = base.strip_suffix("/api/v4").unwrap_or(base);
        let roots = selection["paths"].as_array();
        meta["source_type"] == "gitlab"
          && meta["source_resource_id"] == project_id
          && project_id.parse::<u64>().is_ok_and(|id| id > 0)
          // The ingestion pipeline may replace the title with the filename or
          // a heading. The native source URL retains the project's namespace.
          && (project == project_id || meta["gitlab_url"] == format!("{web_base}/{project}/-/blob/{branch}/{path}"))
          && !path.is_empty()
          && !path.starts_with('/')
          && !path.chars().any(|c| c.is_control() || c == '\\')
          && path.split('/').all(|part| !matches!(part, "" | "." | ".."))
          && !branch.is_empty()
          && (selection["ref"] == "" || selection["ref"] == branch)
          && id == format!("gitlab:{canonical}:{project_id}:{branch}:{path}")
          && roots.is_some_and(|roots| {
            roots.is_empty()
              || roots.iter().any(|root| {
                root
                  .as_str()
                  .is_some_and(|root| root.is_empty() || path.starts_with(&format!("{root}/")))
              })
          })
      },
      _ => false,
    }
  }

  pub async fn markdown(&self, note: &WorkspaceNote) -> Result<String> {
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
    let content = String::from_utf8(bytes)?;
    if note.plain_text {
      // Keep TXT contents literal when passing them through the Markdown importer.
      return Ok(
        content
          .lines()
          .map(|line| format!("    {line}\n"))
          .collect(),
      );
    }
    Ok(content)
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
  fn only_selected_connector_documents_are_eligible() {
    let source_id = Uuid::new_v4();
    let mut source = WorkspaceNoteSource {
      client: Client::new("http://localhost".into(), "http://localhost".into()).unwrap(),
      key: String::new(),
      kb: "base".into(),
      source_id,
      kind: "feishu".into(),
      resources: vec![json!("selected")],
      projects: Value::Null,
      base_url: Value::Null,
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
    let workspace = Uuid::new_v4();
    assert_eq!(
      source.page_namespace(workspace, 42),
      Uuid::new_v5(&workspace, format!("feishu-wiki:42:{source_id}").as_bytes())
    );

    let page = Uuid::new_v4();
    source.kind = "notion".into();
    source.resources = vec![json!(page)];
    let mut notion = json!({"knowledge_base_id":"base", "file_name":"Page.md", "metadata": {
      "datasource_id":source_id, "object_type":"page", "external_id":page, "source_resource_id":""}});
    assert!(source.matches(&notion));
    notion["metadata"]["object_type"] = json!("database");
    assert!(!source.matches(&notion));
    notion["metadata"]["object_type"] = json!("page");
    notion["metadata"]["external_id"] = json!(Uuid::new_v4());
    assert!(!source.matches(&notion));

    source.kind = "yuque".into();
    source.resources = vec![json!("31")];
    let mut yuque = json!({"knowledge_base_id":"base", "file_name":"Book/Doc.md", "metadata": {
      "datasource_id":source_id, "external_id":"42", "doc_id":"42", "book_id":"31", "source_resource_id":"31"}});
    assert!(source.matches(&yuque));
    yuque["metadata"]["source_resource_id"] = json!("32");
    yuque["metadata"]["book_id"] = json!("32");
    assert!(!source.matches(&yuque));
  }

  #[test]
  fn gitlab_selection_rejects_out_of_scope_and_non_text_files() {
    let source_id = Uuid::new_v4();
    let mut payload = json!({"credentials":{"base_url":"https://gitlab.com/"},
      "settings":{"projects":[{"project_id":"group/project", "ref":"main", "paths":["/docs/"]}]}});
    gitlab_scope(&mut payload).unwrap();
    assert_eq!(payload["settings"]["projects"][0]["paths"], json!(["docs"]));
    assert_eq!(payload["resource_ids"], json!(["group/project"]));
    for path in [
      "../secret",
      "docs/../secret",
      "docs//nested",
      "docs\\secret",
    ] {
      let mut invalid = payload.clone();
      invalid["settings"]["projects"][0]["paths"] = json!([path]);
      assert!(gitlab_scope(&mut invalid).is_err());
    }
    for url in [
      "file:///etc/passwd",
      "https://token@gitlab.com",
      "https://gitlab.com?token=secret",
    ] {
      let mut invalid = payload.clone();
      invalid["credentials"]["base_url"] = json!(url);
      assert!(gitlab_scope(&mut invalid).is_err());
    }
    let mut source = WorkspaceNoteSource {
      client: Client::new("http://localhost".into(), "http://localhost".into()).unwrap(),
      key: String::new(),
      kb: "base".into(),
      source_id,
      kind: "gitlab".into(),
      resources: vec![json!("group/project")],
      projects: payload["settings"]["projects"].clone(),
      base_url: json!("https://gitlab.com"),
    };
    let doc = json!({"knowledge_base_id":"base", "id":Uuid::new_v4(),
      "file_name":"project-main/docs/Guide.md", "title":"Parsed heading", "metadata": {
        "datasource_id":source_id, "source_type":"gitlab", "source_resource_id":"42",
        "gitlab_project_id":"42", "gitlab_ref":"main", "gitlab_path":"docs/Guide.md",
        "gitlab_url":"https://gitlab.com/group/project/-/blob/main/docs/Guide.md",
        "external_id":"gitlab:https://gitlab.com/api/v4:42:main:docs/Guide.md"}});
    assert!(source.matches(&doc));
    for (key, value) in [
      ("datasource_id", json!(Uuid::new_v4())),
      ("gitlab_project_id", json!("43")),
      ("source_resource_id", json!("43")),
      ("gitlab_ref", json!("dev")),
      ("gitlab_path", json!("docs-old/Guide.md")),
      (
        "gitlab_url",
        json!("https://gitlab.com/other/project/-/blob/main/docs/Guide.md"),
      ),
      (
        "external_id",
        json!("gitlab:https://other.example/api/v4:42:main:docs/Guide.md"),
      ),
    ] {
      let mut other = doc.clone();
      other["metadata"][key] = value;
      assert!(!source.matches(&other), "{key}");
    }
    for (path, accepted) in [
      ("docs/nested/README.MD", true),
      ("docs/a.markdown", true),
      ("docs/a.txt", true),
      ("docs/a.pdf", false),
      ("docs-old/a.md", false),
      ("docs/../a.md", false),
    ] {
      let mut other = doc.clone();
      other["file_name"] = json!(format!("project-main/{path}"));
      other["title"] = json!(format!("group/project/{path}"));
      other["metadata"]["gitlab_path"] = json!(path);
      other["metadata"]["gitlab_url"] = json!(format!(
        "https://gitlab.com/group/project/-/blob/main/{path}"
      ));
      other["metadata"]["external_id"] =
        json!(format!("gitlab:https://gitlab.com/api/v4:42:main:{path}"));
      assert_eq!(source.matches(&other), accepted, "{path}");
    }
    source.projects[0]["project_id"] = json!("42");
    assert!(source.matches(&doc));
    let workspace = Uuid::new_v4();
    assert_eq!(
      source.page_namespace(workspace, 1),
      source.page_namespace(workspace, 1)
    );
    assert_ne!(
      source.page_namespace(workspace, 1),
      source.page_namespace(workspace, 2)
    );
  }

  #[tokio::test]
  async fn gitlab_connection_persists_scope_and_checks_it_before_import() {
    let staging = Uuid::new_v4().to_string();
    let request = Uuid::new_v4();
    let sid = Uuid::new_v5(
      &Uuid::NAMESPACE_URL,
      format!("ponynotes:datasource:{staging}:{request}").as_bytes(),
    );
    let path = format!("/api/v1/datasource/{sid}");
    let scope = json!({"base_url":"https://gitlab.com", "projects":[{"project_id":"42", "ref":"main", "paths":["docs"]}]});
    let source = json!({"id":sid,"knowledge_base_id":staging,"name":"GitLab docs","type":"gitlab",
      "status":"paused", "config":{"resource_ids":["42"],"settings":scope,"credentials":{"access_token":"secret"}}});
    let base = json!({"data":{"id":staging,"name":STAGING_NAME,"indexing_strategy":{
      "vector_enabled":false,"keyword_enabled":false,"wiki_enabled":false,"graph_enabled":false}}});
    let mut changed = source.clone();
    changed["config"]["settings"]["projects"][0]["ref"] = json!("dev");
    let (client, task, requests) = crate::tests::mock_recorded(vec![
      (
        "GET /api/v1/datasource/types ".into(),
        200,
        json!([{"type":"gitlab"},{"type":"feishu"},{"type":"rss"}]),
      ),
      (
        format!("GET /api/v1/knowledge-bases/{staging} "),
        200,
        base.clone(),
      ),
      (format!("GET {path} "), 404, json!({})),
      ("POST /api/v1/datasource ".into(), 201, source.clone()),
      (
        format!("GET /api/v1/knowledge-bases/{staging} "),
        200,
        base.clone(),
      ),
      (
        format!("GET /api/v1/datasource?kb_id={staging} "),
        200,
        json!([source]),
      ),
      (
        format!("GET /api/v1/knowledge-bases/{staging} "),
        200,
        base.clone(),
      ),
      (format!("GET {path} "), 200, source.clone()),
      (format!("GET {path} "), 200, source.clone()),
      (format!("GET {path} "), 200, changed),
      (
        format!("GET /api/v1/knowledge-bases/{staging} "),
        200,
        base.clone(),
      ),
      (format!("GET {path} "), 200, source.clone()),
      (format!("GET /api/v1/knowledge-bases/{staging} "), 200, base),
      (format!("GET {path} "), 200, source.clone()),
      (format!("GET {path} "), 200, source),
      (format!("PUT {path}/credentials "), 200, json!({})),
    ]);
    let sources = WorkspaceNoteSources {
      client,
      key: "server-key".into(),
      staging,
    };
    let types = sources
      .execute(&json!({"operation":"types"}))
      .await
      .unwrap();
    assert_eq!(types["types"].as_array().unwrap().len(), 2);
    assert!(types["types"][0]["fields"]
      .as_array()
      .unwrap()
      .iter()
      .any(|f| f["key"] == "base_url"));
    assert!(!types["types"][1]["fields"]
      .as_array()
      .unwrap()
      .iter()
      .any(|f| f["key"] == "base_url"));
    let created = sources
      .execute(
        &json!({"operation":"create","type":"gitlab","name":"GitLab docs",
      "request_id":request,"credentials":{"base_url":"https://gitlab.com/","access_token":"secret"},
      "settings":{"projects":scope["projects"]}, "resource_ids":["ignored-broader-scope"]}),
      )
      .await
      .unwrap();
    assert_eq!(created["source"]["config"]["settings"], scope);
    assert!(!created.to_string().contains("secret"));
    let reopened = sources.execute(&json!({"operation":"list"})).await.unwrap();
    assert_eq!(reopened["sources"][0], created["source"]);
    let opened = sources.source(&json!({"source_id":sid})).await.unwrap();
    opened.check_scope().await.unwrap();
    assert_eq!(
      crate::library::status(&opened.check_scope().await.unwrap_err()),
      409
    );
    let mut credentials = json!({"operation":"credentials", "source_id":sid,
      "credentials":{"base_url":"https://other.example", "access_token":"replacement"}});
    let error = sources.execute(&credentials).await.unwrap_err();
    assert_eq!(crate::library::status(&error), 400);
    credentials["credentials"]["base_url"] = scope["base_url"].clone();
    assert_eq!(
      sources.execute(&credentials).await.unwrap(),
      json!({"updated":true})
    );
    task.join().unwrap();
    let requests = requests.lock().unwrap();
    let config = &requests
      .iter()
      .find(|r| r.0.starts_with("POST /api/v1/datasource "))
      .unwrap()
      .1["config"];
    assert_eq!(config["settings"], scope);
    assert_eq!(config["resource_ids"], json!(["42"]));
  }
}
