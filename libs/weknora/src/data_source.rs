//! Library-scoped access to WeKnora's native connectors and sync jobs.
use crate::library::{require, uuid, Rejected};
use crate::{array, not_found, optional_array, Client};
use anyhow::{anyhow, Result};
use reqwest::Method;
use serde_json::{json, Value};
use std::collections::HashSet;
use uuid::Uuid;

// Same credential names as WeKnora v0.9.0's DataSourceEditorDialog.
fn fields(kind: &str) -> &'static [(&'static str, &'static str, bool, bool)] {
  match kind {
    "feishu" | "lark" | "feishu_drive" | "lark_drive" => &[
      ("app_id", "App ID", true, false),
      ("app_secret", "App Secret", true, true),
      ("base_url", "API 地址（可选）", false, false),
    ],
    "notion" => &[("api_key", "Integration Token", true, true)],
    "confluence" => &[
      ("edition", "版本：server 或 cloud", true, false),
      ("base_url", "Confluence 地址", true, false),
      ("username", "用户名或邮箱", false, false),
      ("token", "Server/DC PAT（可选）", false, true),
      ("password", "Server/DC 密码（可选）", false, true),
      ("api_token", "Cloud API Token（可选）", false, true),
    ],
    "yuque" => &[
      ("api_token", "API Token", true, true),
      ("base_url", "API 地址（可选）", false, false),
    ],
    "dingtalk" => &[
      ("client_id", "Client ID", true, false),
      ("client_secret", "Client Secret", true, true),
      ("operator_id", "操作用户 ID", true, false),
    ],
    "ima" => &[
      ("client_id", "Client ID", true, true),
      ("api_key", "API Key", true, true),
      ("base_url", "API 地址（可选）", false, false),
    ],
    "rss" => &[(
      "auth_headers",
      "认证请求头（可选，每行 Name: Value）",
      false,
      true,
    )],
    "gitlab" => &[
      ("base_url", "GitLab 地址", true, false),
      ("access_token", "Access Token", true, true),
    ],
    "seafile" => &[
      ("base_url", "Seafile 地址", true, false),
      ("api_token", "API Token", true, true),
    ],
    _ => &[],
  }
}

const SETTINGS: &[&str] = &[
  "parse_mode",
  "folder_mode",
  "toc_only",
  "include_uploaded_files",
  "edition",
  "base_url",
  "username",
  "feed_urls",
  "projects",
];

fn pick(value: &Value, fields: &[&str]) -> Value {
  Value::Object(
    fields
      .iter()
      .map(|key| ((*key).into(), value[*key].clone()))
      .collect(),
  )
}

fn log_info(log: &Value) -> Value {
  let mut result = pick(
    log,
    &[
      "id",
      "status",
      "started_at",
      "finished_at",
      "items_total",
      "items_created",
      "items_updated",
      "items_deleted",
      "items_skipped",
      "items_failed",
    ],
  );
  // Upstream error messages and cursors can contain credentials.
  result["has_error"] = json!(log["error_message"].as_str().is_some_and(|s| !s.is_empty()));
  result
}

fn source_info(source: &Value) -> Value {
  let mut result = pick(
    source,
    &[
      "id",
      "name",
      "type",
      "status",
      "sync_schedule",
      "sync_mode",
      "conflict_strategy",
      "sync_deletions",
      "last_sync_at",
      "total_items_synced",
    ],
  );
  let settings = source["config"]["settings"]
    .as_object()
    .into_iter()
    .flatten()
    .filter(|(key, value)| {
      SETTINGS.contains(&key.as_str()) && (value.is_string() || value.is_boolean())
    })
    .map(|(key, value)| (key.clone(), value.clone()))
    .collect();
  result["config"] = json!({
    "resource_ids":source["config"].get("resource_ids").filter(|v|v.is_array()).cloned().unwrap_or(json!([])),
    "settings":Value::Object(settings),
  });
  result["credentials_configured"] =
    json!(source["credentials"]["credentials"]["configured"] == true);
  result["latest_sync_log"] = if source["latest_sync_log"].is_object() {
    log_info(&source["latest_sync_log"])
  } else {
    Value::Null
  };
  result
}

fn credentials(kind: &str, payload: &Value) -> Result<Value> {
  let fields = fields(kind);
  require(!fields.is_empty(), 400)?;
  let values = payload.get("credentials").cloned().unwrap_or(json!({}));
  let values = values.as_object().ok_or_else(|| anyhow!(Rejected(400)))?;
  require(
    values.iter().all(|(key, value)| {
      fields.iter().any(|field| key == field.0)
        && value.as_str().is_some_and(|s| s.chars().count() <= 4096)
    }),
    400,
  )?;
  require(
    fields.iter().all(|(key, _, required, _)| {
      !required
        || values
          .get(*key)
          .and_then(Value::as_str)
          .is_some_and(|s| !s.trim().is_empty())
    }),
    400,
  )?;
  Ok(json!(values))
}

fn config(payload: &Value) -> Result<Value> {
  let settings = payload.get("settings").cloned().unwrap_or(json!({}));
  require(
    settings
      .as_object()
      .is_some_and(|s| s.keys().all(|k| SETTINGS.contains(&k.as_str()))),
    400,
  )?;
  let ids = payload.get("resource_ids").cloned().unwrap_or(json!([]));
  let ids = ids.as_array().ok_or_else(|| anyhow!(Rejected(400)))?;
  let mut unique = HashSet::new();
  require(
    ids.len() <= 50
      && ids.iter().all(|id| {
        id.as_str().is_some_and(|s| {
          (1..=2048).contains(&s.chars().count())
            && !s.chars().any(char::is_control)
            && unique.insert(s)
        })
      }),
    400,
  )?;
  // Connector-specific settings and scope validation belong to WeKnora.
  Ok(json!({"settings":settings,"resource_ids":ids}))
}

pub(crate) async fn execute(
  client: &Client,
  key: &str,
  kb: &str,
  payload: &Value,
) -> Result<Value> {
  let operation = payload["operation"]
    .as_str()
    .ok_or_else(|| anyhow!(Rejected(400)))?;
  if operation == "types" {
    let result = client
      .weknora(key, "datasource/types", Method::GET, None)
      .await?;
    let types = optional_array(&result)?
      .into_iter()
      .filter_map(|item| {
        let fields = fields(item["type"].as_str()?);
        if fields.is_empty() {
          return None;
        }
        let mut result = pick(item, &["type", "name", "description", "capabilities"]);
        result["fields"] = json!(fields.iter().map(|(key, label, required, secret)|
        json!({"key":key,"label":label,"required":required,"secret":secret})).collect::<Vec<_>>());
        Some(result)
      })
      .collect::<Vec<_>>();
    return Ok(json!({"types":types}));
  }
  if operation == "list" {
    let mut url = reqwest::Url::parse("http://internal/datasource")?;
    url.query_pairs_mut().append_pair("kb_id", kb);
    let result = client
      .weknora(
        key,
        &format!("datasource?{}", url.query().unwrap()),
        Method::GET,
        None,
      )
      .await?;
    let sources = optional_array(&result)?
      .into_iter()
      .filter(|s| s["knowledge_base_id"] == kb)
      .map(source_info)
      .collect::<Vec<_>>();
    return Ok(json!({"sources":sources}));
  }
  if matches!(operation, "test" | "create") {
    let kind = payload["type"]
      .as_str()
      .ok_or_else(|| anyhow!(Rejected(400)))?;
    let mut credentials = credentials(kind, payload)?;
    let mut config = config(payload)?;
    if operation == "test" {
      // The credentials-only native validator expects RSS URLs in this bag.
      if kind == "rss" {
        credentials["feed_urls"] = config["settings"]["feed_urls"].clone();
      }
      client
        .weknora(
          key,
          "datasource/validate-credentials",
          Method::POST,
          Some(&json!({"type":kind,"credentials":credentials})),
        )
        .await?;
      return Ok(json!({"connected":true}));
    }
    let name = payload["name"]
      .as_str()
      .ok_or_else(|| anyhow!(Rejected(400)))?;
    require(
      (1..=100).contains(&name.trim().chars().count()) && !name.chars().any(char::is_control),
      400,
    )?;
    let request_id = uuid(payload, "request_id")?;
    let sid = Uuid::new_v5(
      &Uuid::NAMESPACE_URL,
      format!("ponynotes:datasource:{kb}:{request_id}").as_bytes(),
    );
    // A stable native ID recovers a successful create whose response was lost.
    let source = match client
      .weknora(key, &format!("datasource/{sid}"), Method::GET, None)
      .await
    {
      Ok(source) => source,
      Err(error) if not_found(&error) => {
        config["credentials"] = credentials;
        config["type"] = json!(kind);
        client
          .weknora(
            key,
            "datasource",
            Method::POST,
            Some(&json!({
              "id":sid,"knowledge_base_id":kb,"type":kind,"name":name.trim(),"config":config,
              "status":"paused","sync_schedule":"","sync_mode":"incremental",
              "conflict_strategy":"overwrite","sync_deletions":false,
            })),
          )
          .await?
      },
      Err(error) => return Err(error),
    };
    require(
      source["knowledge_base_id"] == kb && source["type"] == kind,
      403,
    )?;
    return Ok(json!({"source":source_info(&source)}));
  }
  require(
    matches!(
      operation,
      "save"
        | "credentials"
        | "resources"
        | "validate"
        | "sync"
        | "pause"
        | "resume"
        | "logs"
        | "delete"
    ),
    400,
  )?;
  let sid = uuid(payload, "source_id")?;
  let path = format!("datasource/{sid}");
  let source = client.weknora(key, &path, Method::GET, None).await?;
  require(source["knowledge_base_id"] == kb, 403)?;
  if operation == "save" {
    let config = config(payload)?;
    require(!array(&config["resource_ids"])?.is_empty(), 400)?;
    let schedule = payload.get("sync_schedule").cloned().unwrap_or(json!(""));
    require(
      schedule.as_str().is_some_and(|s| s.chars().count() <= 100)
        && matches!(payload["sync_mode"].as_str(), Some("incremental" | "full"))
        && matches!(
          payload["conflict_strategy"].as_str(),
          Some("overwrite" | "skip")
        )
        && payload["sync_deletions"].is_boolean(),
      400,
    )?;
    let result = client.weknora(key, &format!("{path}/sync-settings"), Method::PATCH, Some(&json!({
      "resource_ids":config["resource_ids"],"sync_schedule":schedule,"sync_mode":payload["sync_mode"],
      "conflict_strategy":payload["conflict_strategy"],"sync_deletions":payload["sync_deletions"],
    }))).await?;
    return Ok(json!({"source":source_info(&result)}));
  }
  if operation == "credentials" {
    let values = credentials(source["type"].as_str().unwrap_or(""), payload)?;
    client
      .weknora(
        key,
        &format!("{path}/credentials"),
        Method::PUT,
        Some(&json!({"credentials":values})),
      )
      .await?;
    return Ok(json!({"updated":true}));
  }
  if operation == "resources" {
    let parent = payload.get("parent_id").cloned().unwrap_or(json!(""));
    let parent = parent.as_str().ok_or_else(|| anyhow!(Rejected(400)))?;
    require(
      parent.chars().count() <= 2048 && !parent.chars().any(char::is_control),
      400,
    )?;
    let mut url = reqwest::Url::parse("http://internal/resources")?;
    url.query_pairs_mut().append_pair("parent_id", parent);
    let result = client
      .weknora(
        key,
        &format!("{path}/resources?{}", url.query().unwrap()),
        Method::GET,
        None,
      )
      .await?;
    let resources = optional_array(&result)?
      .into_iter()
      .map(|item| {
        pick(
          item,
          &["external_id", "name", "type", "parent_id", "has_children"],
        )
      })
      .collect::<Vec<_>>();
    return Ok(json!({"resources":resources}));
  }
  if operation == "logs" {
    let result = client
      .weknora(
        key,
        &format!("{path}/logs?limit=20&offset=0"),
        Method::GET,
        None,
      )
      .await?;
    return Ok(
      json!({"logs":optional_array(&result)?.into_iter().map(log_info).collect::<Vec<_>>()}),
    );
  }
  if matches!(operation, "sync" | "resume") {
    require(
      source["config"]["resource_ids"]
        .as_array()
        .is_some_and(|ids| !ids.is_empty()),
      400,
    )?;
  }
  if operation == "sync" {
    let result = client
      .weknora(
        key,
        &format!("{path}/logs?limit=1&offset=0"),
        Method::GET,
        None,
      )
      .await?;
    require(
      optional_array(&result)?
        .first()
        .is_none_or(|log| log["status"] != "running"),
      409,
    )?;
  }
  if operation == "delete" {
    client.weknora(key, &path, Method::DELETE, None).await?;
    return Ok(json!({"deleted":true}));
  }
  let result = client
    .weknora(
      key,
      &format!("{path}/{operation}"),
      Method::POST,
      Some(&json!({})),
    )
    .await?;
  Ok(if operation == "sync" {
    json!({"log":log_info(&result)})
  } else {
    json!({"updated":true})
  })
}
