//! Per-import parsing choices. Only server metadata selects models and engines.
use crate::library::{require, Rejected};
use crate::{array, text, Client};
use anyhow::{anyhow, Result};
use reqwest::Method;
use serde_json::{json, Value};
use std::collections::HashSet;

async fn tags(client: &Client, key: &str, kb: &str) -> Result<Vec<Value>> {
  let mut tags = Vec::new();
  for page in 1..=100 {
    let response = client
      .weknora(
        key,
        &format!("knowledge-bases/{kb}/tags?page={page}&page_size=100"),
        Method::GET,
        None,
      )
      .await?;
    let batch = array(&response["data"]["data"])?;
    tags.extend(
      batch
        .iter()
        .map(|tag| json!({"id":tag["id"],"name":tag["name"]})),
    );
    if tags.len() >= response["data"]["total"].as_u64().unwrap_or(0) as usize {
      return Ok(tags);
    }
    require(!batch.is_empty(), 503)?;
  }
  Err(anyhow!(Rejected(503)))
}

fn choice(service: &Value, id: &Value, kind: &str) -> bool {
  service["parse_models"]
    .as_array()
    .is_some_and(|models| models.iter().any(|m| m["id"] == *id && m["type"] == kind))
}

fn metadata(service: &Value, name: &str, fields: &[&str]) -> Vec<Value> {
  service[name]
    .as_array()
    .into_iter()
    .flatten()
    .map(|item| {
      Value::Object(
        fields
          .iter()
          .map(|field| ((*field).into(), item[*field].clone()))
          .collect(),
      )
    })
    .collect()
}

pub(crate) fn defaults(base: &Value, service: &Value) -> Value {
  let chunk = &base["chunking_config"];
  let model = |field: &str, kind: &str| {
    let value = &base[field];
    let id = if choice(service, &value["model_id"], kind) {
      value["model_id"].as_str().unwrap_or("")
    } else {
      ""
    };
    json!({"enabled":value["enabled"] == true && !id.is_empty(),"model_id":id})
  };
  let mut vlm = model("vlm_config", "VLLM");
  for field in ["description_language", "custom_instructions"] {
    vlm[field] = json!(base["vlm_config"][field].as_str().unwrap_or(""));
  }
  let mut asr = model("asr_config", "ASR");
  asr["language"] = json!(base["asr_config"]["language"].as_str().unwrap_or(""));
  let engine = chunk["parser_engine_rules"]
    .as_array()
    .and_then(|rules| {
      rules.iter().find(|rule| {
        rule["file_types"]
          .as_array()
          .is_some_and(|types| types.iter().any(|t| t == "html"))
      })
    })
    .and_then(|rule| rule["engine"].as_str())
    .unwrap_or("");
  let engine = if service["parse_engines"]
    .as_array()
    .is_some_and(|items| items.iter().any(|e| e["id"] == engine))
  {
    engine
  } else {
    ""
  };
  let summary = base["summary_model_id"]
    .as_str()
    .is_some_and(|s| !s.is_empty());
  json!({"summary_enabled":summary,"parser_engine_rules":[{"file_types":["html"],"engine":engine}],
    "chunking_config":{"chunk_size":chunk["chunk_size"].as_u64().filter(|n|*n>0).unwrap_or(512),
      "chunk_overlap":chunk["chunk_overlap"].as_u64().unwrap_or(80),"strategy":chunk["strategy"].as_str().filter(|s|!s.is_empty()).unwrap_or("auto"),
      "enable_parent_child":chunk["enable_parent_child"] == true,
      "parent_chunk_size":chunk["parent_chunk_size"].as_u64().filter(|n|*n>0).unwrap_or(4096),
      "child_chunk_size":chunk["child_chunk_size"].as_u64().filter(|n|*n>0).unwrap_or(384)},
    "enable_multimodel":vlm["enabled"],"vlm_config":vlm,"asr_config":asr,
    "question_generation_config":{"enabled":base["question_generation_config"]["enabled"] == true && summary,
      "question_count":base["question_generation_config"]["question_count"].as_u64().filter(|n|(1..=10).contains(n)).unwrap_or(3),
      "custom_instructions":base["question_generation_config"]["custom_instructions"].as_str().unwrap_or("")}})
}

fn only(value: &Value, fields: &[&str]) -> Result<()> {
  require(
    value
      .as_object()
      .is_some_and(|o| o.keys().all(|k| fields.contains(&k.as_str()))),
    400,
  )
}
fn boolean(value: &Value) -> Result<bool> {
  value.as_bool().ok_or_else(|| anyhow!(Rejected(400)))
}
fn number(value: &Value, min: u64, max: u64) -> Result<u64> {
  value
    .as_u64()
    .filter(|n| (min..=max).contains(n))
    .ok_or_else(|| anyhow!(Rejected(400)))
}
fn short_text(value: &Value, max: usize) -> Result<()> {
  require(
    value
      .as_str()
      .is_some_and(|s| s.len() <= max && !s.contains('\0')),
    400,
  )
}

pub(crate) fn validate(config: &Value, base: &Value, service: &Value) -> Result<Value> {
  only(
    config,
    &[
      "summary_enabled",
      "parser_engine_rules",
      "chunking_config",
      "enable_multimodel",
      "vlm_config",
      "asr_config",
      "question_generation_config",
    ],
  )?;
  let summary = boolean(&config["summary_enabled"])?;
  let questions = &config["question_generation_config"];
  only(
    questions,
    &["enabled", "question_count", "custom_instructions"],
  )?;
  let enabled = boolean(&questions["enabled"])?;
  number(&questions["question_count"], 1, 10)?;
  short_text(&questions["custom_instructions"], 4000)?;
  if summary || enabled {
    require(
      base["summary_model_id"]
        .as_str()
        .is_some_and(|s| !s.is_empty()),
      400,
    )?;
  }
  let rules = array(&config["parser_engine_rules"]).map_err(|_| anyhow!(Rejected(400)))?;
  require(rules.len() == 1, 400)?;
  only(&rules[0], &["file_types", "engine"])?;
  require(rules[0]["file_types"] == json!(["html"]), 400)?;
  let engine = rules[0]["engine"]
    .as_str()
    .ok_or_else(|| anyhow!(Rejected(400)))?;
  require(
    engine.is_empty()
      || service["parse_engines"]
        .as_array()
        .is_some_and(|items| items.iter().any(|e| e["id"] == engine)),
    400,
  )?;
  let chunk = &config["chunking_config"];
  only(
    chunk,
    &[
      "chunk_size",
      "chunk_overlap",
      "strategy",
      "enable_parent_child",
      "parent_chunk_size",
      "child_chunk_size",
    ],
  )?;
  let size = number(&chunk["chunk_size"], 100, 4000)?;
  require(number(&chunk["chunk_overlap"], 0, 500)? < size, 400)?;
  require(
    matches!(
      chunk["strategy"].as_str(),
      Some("auto" | "heading" | "heuristic" | "recursive" | "legacy")
    ),
    400,
  )?;
  boolean(&chunk["enable_parent_child"])?;
  let parent = number(&chunk["parent_chunk_size"], 100, 20000)?;
  require(
    number(&chunk["child_chunk_size"], 100, 4000)? <= parent,
    400,
  )?;
  for (field, kind, fields) in [
    (
      "vlm_config",
      "VLLM",
      vec![
        "enabled",
        "model_id",
        "description_language",
        "custom_instructions",
      ],
    ),
    ("asr_config", "ASR", vec!["enabled", "model_id", "language"]),
  ] {
    let value = &config[field];
    only(value, &fields)?;
    let enabled = boolean(&value["enabled"])?;
    let id = value["model_id"]
      .as_str()
      .ok_or_else(|| anyhow!(Rejected(400)))?;
    require(
      (id.is_empty() && !enabled) || choice(service, &value["model_id"], kind),
      400,
    )?;
  }
  short_text(&config["vlm_config"]["description_language"], 32)?;
  short_text(&config["vlm_config"]["custom_instructions"], 4000)?;
  short_text(&config["asr_config"]["language"], 32)?;
  require(
    boolean(&config["enable_multimodel"])? == boolean(&config["vlm_config"]["enabled"])?,
    400,
  )?;
  let mut output = config.clone();
  // Preserve unexposed splitter settings without accepting arbitrary model URLs or credentials.
  let mut chunking = base["chunking_config"]
    .as_object()
    .cloned()
    .unwrap_or_default();
  for (key, value) in chunk.as_object().unwrap() {
    chunking.insert(key.clone(), value.clone());
  }
  output["chunking_config"] = json!(chunking);
  // The parser must preserve explicit zero values and cleared instructions in this snapshot.
  output["replace_defaults"] = json!(true);
  Ok(output)
}

pub(crate) async fn execute(
  client: &Client,
  key: &str,
  kb: &str,
  service: &Value,
  action: &str,
  payload: &Value,
) -> Result<Value> {
  let response = client
    .weknora(key, &format!("knowledge-bases/{kb}"), Method::GET, None)
    .await?;
  let base = &response["data"];
  let defaults = defaults(base, service);
  if action == "web_import_options" {
    return Ok(
      json!({"version":2,"chunk_size":defaults["chunking_config"]["chunk_size"],"chunk_overlap":defaults["chunking_config"]["chunk_overlap"],"strategy":defaults["chunking_config"]["strategy"],
      "process_config":defaults,"tags":tags(client,key,kb).await?,
      "models":metadata(service,"parse_models",&["id","name","type"]),"engines":metadata(service,"parse_engines",&["id","name"]),
      "summary_available":base["summary_model_id"].as_str().is_some_and(|s|!s.is_empty())}),
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
  let mut request = json!({"url":url});
  if let Some(config) = payload.get("process_config") {
    request["process_config"] = validate(config, base, service)?;
    let ids = payload["tag_ids"]
      .as_array()
      .ok_or_else(|| anyhow!(Rejected(400)))?;
    require(ids.len() <= 100 && ids.iter().all(Value::is_string), 400)?;
    let distinct: HashSet<_> = ids.iter().map(|v| v.as_str().unwrap()).collect();
    require(distinct.len() == ids.len(), 400)?;
    if !ids.is_empty() {
      let allowed = tags(client, key, kb).await?;
      require(
        ids.iter().all(|id| allowed.iter().any(|t| t["id"] == *id)),
        403,
      )?;
    }
    request["tag_ids"] = json!(ids);
  } else {
    // Older clients still explicitly confirm their three splitting choices.
    let mut config = defaults;
    for field in ["chunk_size", "chunk_overlap", "strategy"] {
      config["chunking_config"][field] = payload[field].clone();
    }
    let chunk = &config["chunking_config"];
    let size = number(&chunk["chunk_size"], 100, 4000)?;
    require(
      number(&chunk["chunk_overlap"], 0, 500)? < size
        && matches!(
          chunk["strategy"].as_str(),
          Some("auto" | "heading" | "heuristic" | "recursive" | "legacy")
        ),
      400,
    )?;
    let mut chunking = base["chunking_config"]
      .as_object()
      .cloned()
      .unwrap_or_default();
    for field in ["chunk_size", "chunk_overlap", "strategy"] {
      chunking.insert(field.into(), payload[field].clone());
    }
    request["process_config"] = json!({"chunking_config":chunking});
  }
  let entry = client
    .weknora(
      key,
      &format!("knowledge-bases/{kb}/knowledge/url"),
      Method::POST,
      Some(&request),
    )
    .await?;
  Ok(json!({"document_id":text(&entry["data"],"id")?}))
}
