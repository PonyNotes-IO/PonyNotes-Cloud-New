use super::*;
use crate::notes::{document_markdown, note_parts, quick_markdown};
use sha2::Digest;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

#[tokio::test]
async fn data_source_management_uses_library_ownership_and_workspace_write_access() {
  let uid = Uuid::new_v4();
  let wid = Uuid::new_v4().to_string();
  let bid = Uuid::new_v4().to_string();
  let mut session = library::Session {
    uid,
    user_id: 2,
    workspaces: vec![
      json!({"workspace_id":wid,"workspace_name":"Library","owner_uid":1,"role":"Member"}),
    ],
  };
  let config = json!({"bases":{&bid:{"id":bid,"workspace_id":wid,"name":"Owned library","owner_uuid":uid,"visibility":"shared","ready":true}}});
  let state = json!({"service":{"api_key":"server-only-key"},"workspaces":{&wid:{"id":"default-kb","ready":true}}});
  let (client, task) = mock(vec![]);
  for (target, owner, role, expected) in [
    (&wid, 1, "Member", 403),
    (&wid, 2, "Guest", 403),
    (&bid, 1, "Guest", 403),
    (&Uuid::new_v4().to_string(), 2, "Owner", 403),
  ] {
    session.workspaces[0]["owner_uid"] = json!(owner);
    session.workspaces[0]["role"] = json!(role);
    let result = library::read(
      &client,
      &session,
      "token",
      "data_source",
      &json!({"workspace_id":wid,"base_id":target,"operation":"types"}),
      &state,
      &config,
    )
    .await;
    assert_eq!(library::status(&result.unwrap_err()), expected);
  }
  task.join().unwrap();
  session.workspaces[0]["owner_uid"] = json!(1);
  session.workspaces[0]["role"] = json!("Member");
  let (client, task, requests) = mock_recorded(vec![(
    format!("GET /api/v1/datasource?kb_id={bid} "),
    200,
    json!([]),
  )]);
  assert_eq!(
    library::read(
      &client,
      &session,
      "token",
      "data_source",
      &json!({"workspace_id":wid,"base_id":bid,"operation":"list","knowledge_base_id":"forged"}),
      &state,
      &config
    )
    .await
    .unwrap(),
    json!({"sources":[]})
  );
  task.join().unwrap();
  assert!(requests.lock().unwrap()[0]
    .0
    .to_ascii_lowercase()
    .contains("x-api-key: server-only-key"));
  let (client, task) = mock(vec![]);
  let mut not_ready = config.clone();
  not_ready["bases"][&bid]["ready"] = json!(false);
  assert_eq!(
    library::status(
      &library::read(
        &client,
        &session,
        "token",
        "data_source",
        &json!({"workspace_id":wid,"base_id":bid,"operation":"types"}),
        &state,
        &not_ready
      )
      .await
      .unwrap_err()
    ),
    503
  );
  task.join().unwrap();
}

#[tokio::test]
async fn data_source_native_lifecycle_redacts_secrets_and_preserves_explicit_scope() {
  use crate::data_source::execute;
  let kb = Uuid::new_v4().to_string();
  let rid = Uuid::new_v4();
  let sid = Uuid::new_v5(
    &Uuid::NAMESPACE_URL,
    format!("ponynotes:datasource:{kb}:{rid}").as_bytes(),
  );
  let path = format!("/api/v1/datasource/{sid}");
  let secret = "never-return-secret";
  let source = json!({"id":sid,"knowledge_base_id":kb,"name":"My Notion","type":"notion","status":"paused",
    "config":{"resource_ids":[],"credentials":{"api_key":secret},"settings":{"api_key":secret}},
    "credentials":{"credentials":{"configured":true}},"last_sync_cursor":secret,"error_message":secret,
    "latest_sync_log":{"id":"log","status":"success","items_created":1,"error_message":secret}});
  let mut saved = source.clone();
  saved["config"]["resource_ids"] = json!(["space"]);
  saved["sync_schedule"] = json!("0 0 */6 * * *");
  let create = json!({"operation":"create","type":"notion","name":"My Notion","credentials":{"api_key":secret},
    "request_id":rid,"settings":{},"resource_ids":[],"tenant_id":999,"knowledge_base_id":"forged"});
  let save = json!({"operation":"save","source_id":sid,"resource_ids":["space"],"sync_schedule":"0 0 */6 * * *",
    "sync_mode":"incremental","conflict_strategy":"overwrite","sync_deletions":false});
  let mut responses = vec![
    (
      "GET /api/v1/datasource/types ".into(),
      200,
      json!([
      {"type":"notion","name":"Notion","description":"Notion"},{"type":"unknown"}]),
    ),
    (
      "POST /api/v1/datasource/validate-credentials ".into(),
      200,
      json!({"status":"connected"}),
    ),
    (
      "POST /api/v1/datasource/validate-credentials ".into(),
      200,
      json!({"status":"connected"}),
    ),
    (format!("GET {path} "), 404, json!({})),
    ("POST /api/v1/datasource ".into(), 201, source.clone()),
    (format!("GET {path} "), 200, source.clone()),
  ];
  for _ in 0..3 {
    responses.push((format!("GET {path} "), 200, source.clone()));
  }
  responses.extend([
    (format!("GET {path} "), 200, source.clone()),
    (format!("PATCH {path}/sync-settings "), 200, saved.clone()),
    (format!("GET /api/v1/datasource?kb_id={kb} "), 200, json!([saved, {"knowledge_base_id":"foreign"}])),
    (format!("GET {path} "), 200, saved.clone()),
    (format!("GET {path}/resources?parent_id=space%26other "), 200,
      json!([{"external_id":"page","parent_id":"space","name":"Page","has_children":false,"url":secret}])),
    (format!("GET {path} "), 200, saved.clone()),
    (format!("PUT {path}/credentials "), 200, json!({"success":true,"data":{}})),
    (format!("GET {path} "), 200, saved.clone()),
    (format!("GET {path}/logs?limit=1&offset=0 "), 200, json!([])),
    (format!("POST {path}/sync "), 200, json!({"id":"log","status":"running","error_message":secret})),
    (format!("GET {path} "), 200, saved.clone()),
    (format!("GET {path}/logs?limit=1&offset=0 "), 200, json!([{"status":"running"}])),
    (format!("GET {path} "), 200, saved.clone()),
    (format!("GET {path}/logs?limit=20&offset=0 "), 200, json!([{"status":"failed","error_message":secret,"items_failed":1}])),
  ]);
  for operation in ["pause", "resume", "validate"] {
    responses.push((format!("GET {path} "), 200, saved.clone()));
    responses.push((
      format!("POST {path}/{operation} "),
      200,
      json!({"status":"connected"}),
    ));
  }
  for _ in 0..9 {
    responses.push((
      format!("GET {path} "),
      200,
      json!({"knowledge_base_id":"foreign"}),
    ));
  }
  responses.extend([
    (format!("GET {path} "), 200, saved),
    (format!("DELETE {path} "), 204, Value::Null),
  ]);
  let (client, task, requests) = mock_recorded(responses);
  let types = execute(&client, "server-key", &kb, &json!({"operation":"types"}))
    .await
    .unwrap();
  assert_eq!(types["types"].as_array().unwrap().len(), 1);
  assert_eq!(types["types"][0]["fields"][0]["key"], "api_key");
  for payload in [
    json!({"operation":"test","type":[],"credentials":{}}),
    json!({"operation":"test","type":"notion","credentials":{"api_key":secret,"injected":secret}}),
    json!({"operation":"test","type":"notion","credentials":{}}),
    json!({"operation":"create","type":"notion","credentials":{"api_key":secret},"resource_ids":["same","same"]}),
  ] {
    assert_eq!(
      library::status(
        &execute(&client, "server-key", &kb, &payload)
          .await
          .unwrap_err()
      ),
      400
    );
  }
  execute(
    &client,
    "server-key",
    &kb,
    &json!({"operation":"test","type":"notion","credentials":{"api_key":secret}}),
  )
  .await
  .unwrap();
  execute(&client, "server-key", &kb, &json!({"operation":"test","type":"rss","credentials":{},"settings":{"feed_urls":"https://example.test/feed"}})).await.unwrap();
  let created = execute(&client, "server-key", &kb, &create).await.unwrap();
  assert_eq!(created["source"]["id"], sid.to_string());
  assert!(!created.to_string().contains(secret));
  assert_eq!(
    execute(&client, "server-key", &kb, &create).await.unwrap(),
    created
  );
  for payload in [
    json!({"operation":"sync","source_id":sid}),
    json!({"operation":"save","source_id":sid,"resource_ids":[]}),
    json!({"operation":"save","source_id":sid,"resource_ids":["space"],"sync_deletions":"true"}),
  ] {
    assert_eq!(
      library::status(
        &execute(&client, "server-key", &kb, &payload)
          .await
          .unwrap_err()
      ),
      400
    );
  }
  let saved = execute(&client, "server-key", &kb, &save).await.unwrap();
  assert_eq!(saved["source"]["config"]["resource_ids"], json!(["space"]));
  let list = execute(&client, "server-key", &kb, &json!({"operation":"list"}))
    .await
    .unwrap();
  assert_eq!(list["sources"].as_array().unwrap().len(), 1);
  assert!(!list.to_string().contains(secret));
  let resources = execute(
    &client,
    "server-key",
    &kb,
    &json!({"operation":"resources","source_id":sid,"parent_id":"space&other"}),
  )
  .await
  .unwrap();
  assert_eq!(resources["resources"][0]["external_id"], "page");
  assert!(!resources.to_string().contains(secret));
  execute(
    &client,
    "server-key",
    &kb,
    &json!({"operation":"credentials","source_id":sid,"credentials":{"api_key":"replacement"}}),
  )
  .await
  .unwrap();
  assert_eq!(
    execute(
      &client,
      "server-key",
      &kb,
      &json!({"operation":"sync","source_id":sid})
    )
    .await
    .unwrap()["log"]["status"],
    "running"
  );
  assert_eq!(
    library::status(
      &execute(
        &client,
        "server-key",
        &kb,
        &json!({"operation":"sync","source_id":sid})
      )
      .await
      .unwrap_err()
    ),
    409
  );
  let logs = execute(
    &client,
    "server-key",
    &kb,
    &json!({"operation":"logs","source_id":sid}),
  )
  .await
  .unwrap();
  assert_eq!(logs["logs"][0]["items_failed"], 1);
  assert!(!logs.to_string().contains(secret));
  for operation in ["pause", "resume", "validate"] {
    execute(
      &client,
      "server-key",
      &kb,
      &json!({"operation":operation,"source_id":sid}),
    )
    .await
    .unwrap();
  }
  for operation in [
    "resources",
    "validate",
    "sync",
    "pause",
    "resume",
    "logs",
    "delete",
    "save",
    "credentials",
  ] {
    for id in [format!("../{sid}"), sid.to_string()] {
      assert_eq!(
        library::status(
          &execute(
            &client,
            "server-key",
            &kb,
            &json!({"operation":operation,"source_id":id})
          )
          .await
          .unwrap_err()
        ),
        if id.starts_with("../") { 400 } else { 403 }
      );
    }
  }
  assert_eq!(
    execute(
      &client,
      "server-key",
      &kb,
      &json!({"operation":"delete","source_id":sid})
    )
    .await
    .unwrap(),
    json!({"deleted":true})
  );
  task.join().unwrap();
  let requests = requests.lock().unwrap();
  let create = &requests
    .iter()
    .find(|(header, _)| header.starts_with("POST /api/v1/datasource "))
    .unwrap()
    .1;
  assert_eq!(create["knowledge_base_id"], kb);
  assert!(create.get("tenant_id").is_none());
  assert_eq!(create["status"], "paused");
  assert_eq!(create["sync_schedule"], "");
  assert_eq!(create["sync_deletions"], false);
  let patch = &requests
    .iter()
    .find(|(header, _)| header.starts_with("PATCH"))
    .unwrap()
    .1;
  assert_eq!(patch.as_object().unwrap().len(), 5);
  assert!(patch.get("credentials").is_none() && patch.get("status").is_none());
  let rss = &requests[2].1;
  assert_eq!(rss["credentials"]["feed_urls"], "https://example.test/feed");
}

#[tokio::test]
async fn web_import_v2_preserves_explicit_choices_and_rejects_untrusted_settings() {
  let base = json!({"summary_model_id":"summary","chunking_config":{"chunk_size":640,"chunk_overlap":80,"separators":["\n\n"],"enable_parent_child":true},
    "vlm_config":{"enabled":true,"model_id":"vision"},"question_generation_config":{"enabled":true,"question_count":3}});
  let service = json!({"parse_models":[{"id":"vision","name":"Vision","type":"VLLM","parameters":{"api_key":"never-return"}}],
    "parse_engines":[{"id":"builtin","name":"Builtin","credentials":"never-return"}]});
  let mut config = web_import::defaults(&base, &service);
  config["summary_enabled"] = json!(false);
  config["enable_multimodel"] = json!(false);
  config["vlm_config"]["enabled"] = json!(false);
  config["question_generation_config"]["enabled"] = json!(false);
  config["chunking_config"]["chunk_overlap"] = json!(0);
  config["chunking_config"]["enable_parent_child"] = json!(false);
  config["parser_engine_rules"][0]["engine"] = json!("builtin");
  let forwarded = web_import::validate(&config, &base, &service).unwrap();
  assert_eq!(forwarded["chunking_config"]["separators"], json!(["\n\n"]));
  for (pointer, value) in [
    ("/vlm_config/model_id", json!("foreign-model")),
    ("/vlm_config/api_key", json!("injected")),
    ("/parser_engine_rules/0/engine", json!("foreign-engine")),
    ("/chunking_config/chunk_size", json!(99)),
    ("/chunking_config/chunk_overlap", json!(640)),
  ] {
    let mut bad = config.clone();
    if pointer == "/vlm_config/api_key" {
      bad["vlm_config"]["api_key"] = value;
    } else {
      *bad.pointer_mut(pointer).unwrap() = value;
    }
    assert_eq!(
      library::status(&web_import::validate(&bad, &base, &service).unwrap_err()),
      400
    );
  }
  let base_response = json!({"data":base});
  let tag_response = json!({"data":{"data":[{"id":"tag","name":"Learning"}],"total":1}});
  let (client, task, requests) = mock_recorded(vec![
    (
      "GET /api/v1/knowledge-bases/kb ".into(),
      200,
      base_response.clone(),
    ),
    (
      "GET /api/v1/knowledge-bases/kb/tags?page=1&page_size=100 ".into(),
      200,
      tag_response.clone(),
    ),
    (
      "GET /api/v1/knowledge-bases/kb ".into(),
      200,
      base_response.clone(),
    ),
    (
      "GET /api/v1/knowledge-bases/kb/tags?page=1&page_size=100 ".into(),
      200,
      tag_response.clone(),
    ),
    (
      "POST /api/v1/knowledge-bases/kb/knowledge/url ".into(),
      200,
      json!({"data":{"id":"document"}}),
    ),
    ("GET /api/v1/knowledge-bases/kb ".into(), 200, base_response),
    (
      "GET /api/v1/knowledge-bases/kb/tags?page=1&page_size=100 ".into(),
      200,
      tag_response,
    ),
  ]);
  let options = web_import::execute(
    &client,
    "key",
    "kb",
    &service,
    "web_import_options",
    &json!({}),
  )
  .await
  .unwrap();
  assert!(!options.to_string().contains("never-return"));
  let mut payload = json!({"url":"https://example.test","tag_ids":["tag"],"process_config":config});
  assert_eq!(
    web_import::execute(&client, "key", "kb", &service, "import_url", &payload)
      .await
      .unwrap()["document_id"],
    "document"
  );
  payload["tag_ids"] = json!(["foreign"]);
  assert_eq!(
    library::status(
      &web_import::execute(&client, "key", "kb", &service, "import_url", &payload)
        .await
        .unwrap_err()
    ),
    403
  );
  task.join().unwrap();
  let requests = requests.lock().unwrap();
  assert_eq!(
    requests[4].1,
    json!({"url":"https://example.test","tag_ids":["tag"],"process_config":forwarded})
  );
  assert_eq!(
    requests.iter().filter(|r| r.0.starts_with("POST")).count(),
    1
  );
  assert_eq!(base["chunking_config"]["chunk_overlap"], 80);
}

#[tokio::test]
async fn reference_catalog_keeps_uploaded_files_and_filters_hidden_synced_notes() {
  let wid = Uuid::new_v4();
  let current = Uuid::new_v4().to_string();
  let other = Uuid::new_v4().to_string();
  let private = Uuid::new_v4().to_string();
  let shared = Uuid::new_v4().to_string();
  let file = Uuid::new_v4().to_string();
  let current_file = Uuid::new_v4().to_string();
  let shared_file = Uuid::new_v4().to_string();
  let hidden = Uuid::new_v4().to_string();
  let note = Uuid::new_v4().to_string();
  let (client, task) = mock(vec![
    (
      "GET /api/v1/knowledge-bases ".into(),
      200,
      json!({"data":[{"id":current,"name":"current"},{"id":other,"name":"hidden workspace"},{"id":private,"name":"private files"}]}),
    ),
    (
      "GET /api/v1/shared-knowledge-bases ".into(),
      200,
      json!({"data":[{"knowledge_base":{"id":shared,"name":"shared files"}}]}),
    ),
    (
      format!("GET /api/v1/knowledge-bases/{current}/knowledge?page=1&page_size=100"),
      200,
      json!({"total":1,"data":[{"id":current_file,"type":"file","file_name":"workspace.pdf"}]}),
    ),
    (
      format!("GET /api/v1/knowledge-bases/{private}/knowledge?page=1&page_size=100"),
      200,
      json!({"total":2,"data":[{"id":file,"type":"file","file_name":"report.pdf"},{"id":hidden,"title":"hidden note","channel":format!("pn:{}","a".repeat(40))}]}),
    ),
    (
      format!("GET /api/v1/knowledge-bases/{shared}/knowledge?page=1&page_size=100"),
      200,
      json!({"total":1,"data":[{"id":shared_file,"type":"file","file_name":"shared.pdf"}]}),
    ),
  ]);
  let scope = agent::Scope {
    token: "token".into(),
    workspace_name: "notes".into(),
    documents: std::collections::BTreeMap::new(),
    notes: std::collections::BTreeMap::from([(
      note.clone(),
      json!({"title":"unsynced note","path":"Private","url":format!("ponynotes://open?viewId={note}&workspaceId={wid}")}),
    )]),
  };
  let account = json!({"workspaces":{wid.to_string():{"id":current},"other":{"id":other}}});
  let catalog = client
    .reference_catalog(&account, wid, &scope)
    .await
    .unwrap();
  assert!(!catalog.sources.contains_key(&hidden));
  assert!(catalog
    .items
    .iter()
    .any(|i| i["kind"] == "file" && i["id"] == current_file));
  assert_eq!(
    catalog
      .items
      .iter()
      .filter(|i| i["kind"] == "base" && i["id"] == current)
      .count(),
    1
  );
  assert!(!catalog.items.iter().any(|i| i["id"] == other));
  let selected = catalog
    .resolve(
      &[references::Reference {
        kind: "base".into(),
        id: shared,
      }],
      true,
    )
    .unwrap();
  assert_eq!(
    selected.keys().cloned().collect::<Vec<_>>(),
    vec![shared_file]
  );
  let note_sources = catalog
    .resolve(
      &[references::Reference {
        kind: "note".into(),
        id: note.clone(),
      }],
      true,
    )
    .unwrap();
  assert!(note_sources.contains_key(&format!("note:{note}")));
  assert!(catalog
    .resolve(
      &[references::Reference {
        kind: "file".into(),
        id: hidden
      }],
      true
    )
    .is_err());
  task.join().unwrap();
}

#[tokio::test]
async fn independent_conversation_needs_workspace_access_but_no_note_document() {
  let uid = Uuid::new_v4();
  let wid = Uuid::new_v4();
  let account = json!({"email":"test@example.test","password":"test-password","user_id":"user","tenant_id":7,"api_key":"sync-key","workspaces":{}});
  let (client, task) = mock(vec![
    ("GET /gotrue/user".into(), 200, json!({"id":uid})),
    (
      "GET /api/workspace ".into(),
      200,
      json!({"data":[{"workspace_id":wid}]}),
    ),
    (
      format!("GET /api/workspace/{wid}/folder?depth=1&root_view_id={wid}"),
      200,
      json!({"data":{"view_id":wid,"name":"root","children":[]}}),
    ),
    (
      "POST /api/v1/auth/login".into(),
      200,
      json!({"token":"independent-token","user":{"id":"user"},"active_tenant":{"id":7}}),
    ),
  ]);
  let scope = client
    .conversation_scope(&account, uid, "cloud-token", wid)
    .await
    .unwrap();
  assert_eq!(scope.token, "independent-token");
  assert!(scope.documents.is_empty());
  task.join().unwrap();
}

#[tokio::test]
async fn smart_reasoning_uses_builtin_tuning_and_provisions_a_user_sandbox() {
  let (client, task) = mock(vec![
    (
      "GET /api/v1/agents/builtin-smart-reasoning".into(),
      200,
      json!({"data":{"config":{"agent_mode":"smart-reasoning","max_iterations":50,"max_completion_tokens":0,"allowed_tools":["thinking","search_knowledge","search_conversations"]}}}),
    ),
    (
      "GET /api/v1/sandbox-configs".into(),
      200,
      json!({"data":[]}),
    ),
    (
      "POST /api/v1/sandbox-configs".into(),
      200,
      json!({"data":{"id":"isolated-docker"}}),
    ),
  ]);
  let cfg = client
    .agent_config("token", "smart-reasoning", "own-model", true, false)
    .await
    .unwrap();
  assert_eq!(cfg["sandbox_config_id"], "isolated-docker");
  assert_eq!(cfg["max_iterations"], 50);
  assert_eq!(cfg["max_completion_tokens"], 0);
  assert_eq!(
    cfg["allowed_tools"],
    json!(["thinking", "search_knowledge"])
  );
  task.join().unwrap();
}

#[tokio::test]
async fn quick_answer_keeps_rag_defaults_without_provisioning_a_sandbox() {
  let (client, task) = mock(vec![(
    "GET /api/v1/agents/builtin-quick-answer".into(),
    200,
    json!({"data":{"config":{"agent_mode":"quick-answer","enable_rewrite":true,"faq_score_boost":1.2,"sandbox_config_id":"must-not-execute"}}}),
  )]);
  let cfg = client
    .agent_config("token", "quick-answer", "own-model", false, true)
    .await
    .unwrap();
  assert_eq!(cfg["sandbox_config_id"], "");
  assert_eq!(cfg["enable_rewrite"], true);
  assert_eq!(cfg["faq_score_boost"], 1.2);
  task.join().unwrap();
}

#[tokio::test]
async fn agent_login_binds_identity_and_limits_knowledge_to_visible_pages() {
  let uid = Uuid::new_v4();
  let wid = Uuid::new_v4();
  let page = Uuid::new_v4();
  let hidden = Uuid::new_v4();
  let entry = |id: Uuid| json!({"id":id,"knowledge_base_id":"kb","title":"笔记","channel":format!("pn:{}","a".repeat(40)),"metadata":{"content":format!("ponynotes://open?viewId={id}&workspaceId={wid}")}});
  let account = json!({"email":"test@example.test","password":"test-password","user_id":"user","tenant_id":7,"api_key":"sync-key","workspaces":{wid.to_string():{"id":"kb"}}});
  let (client, task) = mock(vec![
    ("GET /gotrue/user".into(), 200, json!({"id":uid})),
    (
      "GET /api/workspace ".into(),
      200,
      json!({"data":[{"workspace_id":wid}]}),
    ),
    (
      format!("GET /api/workspace/{wid}/folder?depth=1&root_view_id={wid}"),
      200,
      json!({"data":{"view_id":wid,"name":"root","children":[{"view_id":page}]}}),
    ),
    (
      format!("GET /api/workspace/{wid}/folder?depth=1&root_view_id={page}"),
      200,
      json!({"data":{"view_id":page,"name":"note","children":[]}}),
    ),
    (
      "POST /api/v1/auth/login".into(),
      200,
      json!({"token":"agent-token","user":{"id":"user"},"active_tenant":{"id":7}}),
    ),
    (
      "GET /api/v1/knowledge-bases/kb/knowledge?".into(),
      200,
      json!({"data":[entry(page),entry(hidden)],"total":2}),
    ),
  ]);
  let scope = client
    .agent_scope(&account, uid, "cloud-token", wid, page)
    .await
    .unwrap();
  assert_eq!(scope.token, "agent-token");
  assert_eq!(scope.documents.len(), 1);
  assert!(scope.documents.contains_key(&page.to_string()));
  task.join().unwrap();
}

#[tokio::test]
async fn agent_history_reads_all_pages_in_order() {
  let session = Uuid::new_v4();
  let page:Vec<Value>=(1..=100).rev().map(|i|json!({"id":format!("{i:03}"),"created_at":format!("2026-11-11T00:00:00.{i:03}Z"),"role":"assistant","is_completed":true})).collect();
  let (client, task) = mock(vec![
    (
      format!("GET /api/v1/messages/{session}/load?limit=100 "),
      200,
      json!({"data":page}),
    ),
    (
      format!("GET /api/v1/messages/{session}/load?limit=100&before_time="),
      200,
      json!({"data":[{"id":"000","created_at":"2026-11-11T00:00:00.000Z","role":"user"}]}),
    ),
  ]);
  let history = client
    .agent_history("agent-token", &session.to_string())
    .await
    .unwrap();
  assert_eq!(history.len(), 101);
  assert_eq!(history[0]["id"], "000");
  assert_eq!(history[100]["id"], "100");
  task.join().unwrap();
}

/// Tiny local HTTP server; exercises real requests without credentials or external services.
fn mock(responses: Vec<(String, u16, Value)>) -> (Client, thread::JoinHandle<()>) {
  let (client, task, _) = mock_recorded(responses);
  (client, task)
}

type Requests = std::sync::Arc<std::sync::Mutex<Vec<(String, Value)>>>;
fn mock_recorded(
  responses: Vec<(String, u16, Value)>,
) -> (Client, thread::JoinHandle<()>, Requests) {
  let listener = TcpListener::bind("127.0.0.1:0").unwrap();
  let address = format!("http://{}", listener.local_addr().unwrap());
  let requests: Requests = Default::default();
  let captured = requests.clone();
  let task = thread::spawn(move || {
    for (expected, status, payload) in responses {
      let (mut stream, _) = listener.accept().unwrap();
      stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
      let mut data = Vec::new();
      let mut byte = [0u8; 1];
      while !data.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).unwrap();
        data.push(byte[0]);
      }
      let header = String::from_utf8(data).unwrap();
      assert!(
        header.starts_with(&expected),
        "expected {expected}; got {}",
        header.lines().next().unwrap()
      );
      let length = header
        .lines()
        .find_map(|line| {
          line
            .to_ascii_lowercase()
            .strip_prefix("content-length: ")
            .and_then(|v| v.parse::<usize>().ok())
        })
        .unwrap_or(0);
      let mut body = vec![0u8; length];
      stream.read_exact(&mut body).unwrap();
      captured
        .lock()
        .unwrap()
        .push((header, serde_json::from_slice(&body).unwrap_or(Value::Null)));
      let body = if status == 204 {
        String::new()
      } else {
        payload.to_string()
      };
      write!(stream,"HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
    }
  });
  (
    Client::new(address.clone() + "/api/v1/", address).unwrap(),
    task,
    requests,
  )
}

#[test]
fn exports_live_blocks_and_splits_without_losing_text() {
  let mut doc = json!({"page_id":"root","blocks":{
    "root":{"ty":"page","children":"ordered"},
    "a":{"ty":"heading","external_id":"ta","data":"{\"level\":2}"},
    "b":{"ty":"paragraph","external_id":"tb"},
    "deleted":{"ty":"paragraph","external_id":"private"}},
    "meta":{"children_map":{"ordered":["a","b"]},"text_map":{"ta":"标题","tb":"正文","private":"deleted"}}});
  assert_eq!(document_markdown(&doc).unwrap(), "## 标题\n\n正文");
  doc["meta"]["children_map"]["ordered"]
    .as_array_mut()
    .unwrap()
    .push(json!("root"));
  assert!(document_markdown(&doc).is_err());
  assert_eq!(
    quick_markdown(&json!({"type":"paragraph","attributes":{"delta":[{"insert":"内容"}]}}))
      .unwrap(),
    "内容\n"
  );
  let body = "笔记😀".repeat(140000);
  let parts = note_parts("w", "document", "d", "Title", &body, "", None).unwrap();
  assert_eq!(parts.len(), 3);
  assert!(parts
    .values()
    .all(|p| p["content"].as_str().unwrap().chars().count() <= 190000));
  let empty = note_parts("w", "document", "d", "Title", "", "", None).unwrap();
  let header = empty.values().next().unwrap()["content"]
    .as_str()
    .unwrap()
    .strip_suffix("（空白笔记）")
    .unwrap();
  let mut expected_parts = (0..3).map(|i| {
    let key = format!(
      "pn:{:x}",
      sha2::Sha256::digest(format!("w/document/d/{i}").as_bytes())
    )[..43]
      .to_owned();
    parts[&key]["content"]
      .as_str()
      .unwrap()
      .strip_prefix(header)
      .unwrap()
      .to_owned()
  });
  assert_eq!(expected_parts.by_ref().collect::<String>(), body);
}

#[test]
fn source_visibility_and_managed_channels_fail_closed() {
  let wid = Uuid::new_v4().to_string();
  let own = Uuid::new_v4().to_string();
  let private = Uuid::new_v4().to_string();
  let visible = HashSet::from([own.clone()]);
  let link = format!("ponynotes://open?viewId={own}&workspaceId={wid}");
  assert_eq!(
    source_url(&format!("[打开]({link})"), &wid, &visible),
    Some(link)
  );
  assert_eq!(
    source_url(
      &format!("ponynotes://open?viewId={private}&workspaceId={wid}"),
      &wid,
      &visible
    ),
    None
  );
  assert_eq!(
    source_url(
      &format!("ponynotes://open?viewId={own}&workspaceId={private}"),
      &wid,
      &visible
    ),
    None
  );
  assert!(!managed("pn:bogus"));
  assert!(managed(&format!("pn:{}", "a".repeat(40))));
}

#[tokio::test]
async fn current_visibility_filters_stale_private_pages_and_deduplicates() {
  let uid = Uuid::new_v4();
  let wid = Uuid::new_v4();
  let own = Uuid::new_v4();
  let private = Uuid::new_v4();
  let channel = format!("pn:{}", "a".repeat(40));
  let folder =
    |id: Uuid, children: Value| json!({"data":{"view_id":id,"name":"page","children":children}});
  let entry = |id: Uuid| json!({"data":{"knowledge_base_id":"kb-a","channel":channel,"title":"Note","metadata":{"content":format!("ponynotes://open?viewId={id}&workspaceId={wid}")}}});
  let responses = vec![
    ("GET /gotrue/user".into(), 200, json!({"id":uid})),
    (
      "GET /api/workspace?include_role=true ".into(),
      200,
      json!({"data":[{"workspace_id":wid}]}),
    ),
    (
      format!("GET /api/workspace/{wid}/folder?depth=1&root_view_id={wid}"),
      200,
      folder(wid, json!([{"view_id":own}])),
    ),
    (
      format!("GET /api/workspace/{wid}/folder?depth=1&root_view_id={own}"),
      200,
      folder(own, json!([])),
    ),
    (
      "GET /api/v1/knowledge-bases/kb-a ".into(),
      200,
      json!({"data":{"embedding_model_id":"embedding"}}),
    ),
    (
      "POST /api/v1/knowledge-bases/kb-a/hybrid-search".into(),
      200,
      json!({"data":[{"knowledge_id":"hidden","content":"secret"},{"knowledge_id":"own","content":"Visible text"},{"knowledge_id":"own","content":"duplicate"}]}),
    ),
    ("GET /api/v1/knowledge/hidden".into(), 200, entry(private)),
    ("GET /api/v1/knowledge/own".into(), 200, entry(own)),
  ];
  let (client, task) = mock(responses);
  let session = library::Session::authenticate(&client, uid, 1, "session")
    .await
    .unwrap();
  let state = json!({"service":{"api_key":"service"},"workspaces":{wid.to_string():{"id":"kb-a","ready":true}}});
  let result = library::read(
    &client,
    &session,
    "session",
    "search",
    &json!({"workspace_id":wid,"query":"query"}),
    &state,
    &json!({}),
  )
  .await
  .unwrap();
  assert_eq!(result["sources"].as_array().unwrap().len(), 1);
  assert_eq!(result["sources"][0]["content"], "Visible text");
  assert!(!result.to_string().contains("secret"));
  task.join().unwrap();
}

#[tokio::test]
async fn failed_write_cannot_delete_existing_notes_and_lost_create_recovers() {
  let desired = note_parts("w", "document", "doc", "Title", "Body", "", None).unwrap();
  let channel = desired.keys().next().unwrap();
  let stale =
    json!({"id":"stale","channel":format!("pn:{}","b".repeat(40)),"parse_status":"completed"});
  let (client, task) = mock(vec![
    (
      "GET /api/v1/knowledge-bases/kb/knowledge?".into(),
      200,
      json!({"data":[stale],"total":1}),
    ),
    (
      "POST /api/v1/knowledge-bases/kb/knowledge/manual".into(),
      500,
      json!({}),
    ),
  ]);
  assert!(
    sync::reconcile(&client, "key", &json!({"id":"kb"}), &desired)
      .await
      .is_err()
  );
  task.join().unwrap();
  // A prior create succeeded but its HTTP response was lost: recover by channel.
  let entry = json!({"id":"already-created","channel":channel,"title":"Title","metadata":{"content":desired[channel]["content"]},"parse_status":"draft"});
  let (client, task) = mock(vec![(
    "GET /api/v1/knowledge-bases/kb/knowledge?".into(),
    200,
    json!({"data":[entry],"total":1}),
  )]);
  let result = sync::reconcile(&client, "key", &json!({"id":"kb"}), &desired)
    .await
    .unwrap();
  assert_eq!(result["unchanged"], 1);
  assert_eq!(result["created"], 0);
  task.join().unwrap();
}

#[tokio::test]
async fn source_failure_stops_scan_before_reconcile() {
  let wid = Uuid::new_v4();
  let page = Uuid::new_v4();
  let (client, task) = mock(vec![
    (
      format!("GET /api/workspace/{wid}/folder?depth=1&root_view_id={wid}"),
      200,
      json!({"data":{"view_id":wid,"name":"root","children":[{"view_id":page}]}}),
    ),
    (
      format!("GET /api/workspace/{wid}/folder?depth=1&root_view_id={page}"),
      503,
      json!({}),
    ),
  ]);
  assert!(client
    .scan("token", &wid.to_string(), "owner")
    .await
    .is_err());
  task.join().unwrap();
}

#[tokio::test]
async fn scan_handles_deep_folders_and_quick_note_pagination() {
  let ids = (0..15).map(|_| Uuid::new_v4()).collect::<Vec<_>>();
  let wid = ids[0];
  let mut responses = Vec::new();
  for (index, id) in ids.iter().enumerate() {
    let children = ids
      .get(index + 1)
      .map(|id| json!([{"view_id":id}]))
      .unwrap_or(json!([]));
    responses.push((format!("GET /api/workspace/{wid}/folder?depth=1&root_view_id={id}"),200,
      json!({"data":{"view_id":id,"name":"nested","children":children,"is_space":index<14,"layout":0}})));
  }
  responses.push((format!("GET /api/workspace/v1/{wid}/collab/{}/json",ids[14]),200,
    json!({"data":{"collab":{"document":{"page_id":"root","blocks":{"root":{"ty":"paragraph","external_id":"t"}},"meta":{"text_map":{"t":"Document"},"children_map":{}}}}}})));
  responses.push((
    format!("GET /api/workspace/{wid}/database"),
    200,
    json!({"data":[]}),
  ));
  for (offset, count) in [(0, 100), (100, 1)] {
    let notes=(offset..offset+count).map(|i|json!({"id":format!("q{i}"),"created_at":"2026-11-06","data":[{"type":"paragraph","delta":[{"insert":format!("Quick {i}")}]}]})).collect::<Vec<_>>();
    responses.push((
      format!("GET /api/workspace/{wid}/quick-note?offset={offset}"),
      200,
      json!({"data":{"quick_notes":notes,"has_more":offset==0}}),
    ));
  }
  let (client, task) = mock(responses);
  let notes = client
    .scan("token", &wid.to_string(), "owner")
    .await
    .unwrap();
  assert_eq!(notes.len(), 102);
  assert!(notes
    .values()
    .any(|n| n["content"].as_str().unwrap().contains("Quick 100")));
  task.join().unwrap();
}

#[tokio::test]
async fn model_promotion_updates_in_place_and_only_prunes_managed_entries() {
  let desired = note_parts("w", "document", "doc", "Title", "Body", "", None).unwrap();
  let channel = desired.keys().next().unwrap();
  let entries = json!([
    {"id":"draft","channel":channel,"title":"Old title","metadata":{"content":"Old body"},"parse_status":"draft"},
    {"id":"stale","channel":format!("pn:{}","b".repeat(40)),"parse_status":"completed"},
    {"id":"manual-user-note","channel":"","parse_status":"completed"}
  ]);
  let (client, task) = mock(vec![
    (
      "GET /api/v1/knowledge-bases/kb/knowledge?".into(),
      200,
      json!({"data":entries,"total":3}),
    ),
    (
      "PUT /api/v1/knowledge/manual/draft".into(),
      200,
      json!({"success":true}),
    ),
    (
      "DELETE /api/v1/knowledge/stale".into(),
      200,
      json!({"success":true}),
    ),
  ]);
  let counts = sync::reconcile(
    &client,
    "tenant-a-key",
    &json!({"id":"kb","embedding_model_id":"model"}),
    &desired,
  )
  .await
  .unwrap();
  assert_eq!(counts["updated"], 1);
  assert_eq!(counts["deleted"], 1);
  assert_eq!(counts["created"], 0);
  task.join().unwrap();
}

#[tokio::test]
async fn forged_identity_and_foreign_workspace_cannot_reach_weknora() {
  let uid = Uuid::new_v4();
  let wid = Uuid::new_v4();
  let (client, task) = mock(vec![(
    "GET /gotrue/user".into(),
    200,
    json!({"id":Uuid::new_v4()}),
  )]);
  assert!(library::Session::authenticate(&client, uid, 1, "token")
    .await
    .is_err());
  task.join().unwrap();
  let (client, task) = mock(vec![
    ("GET /gotrue/user".into(), 200, json!({"id":uid})),
    (
      "GET /api/workspace?include_role=true ".into(),
      200,
      json!({"data":[{"workspace_id":Uuid::new_v4()}]}),
    ),
  ]);
  let session = library::Session::authenticate(&client, uid, 1, "token")
    .await
    .unwrap();
  let result = library::read(
    &client,
    &session,
    "token",
    "search",
    &json!({"workspace_id":wid,"query":"query"}),
    &json!({}),
    &json!({}),
  )
  .await;
  assert_eq!(library::status(&result.unwrap_err()), 403);
  task.join().unwrap();
}

#[tokio::test]
async fn native_library_permissions_import_and_connector_redaction() {
  use library::{read, Session};
  let uid = Uuid::new_v4();
  let wid = Uuid::new_v4();
  let bid = Uuid::new_v4();
  let mut session = Session {
    uid,
    user_id: 2,
    workspaces: vec![
      json!({"workspace_id":wid,"workspace_name":"空间","owner_uid":1,"role":"Member"}),
    ],
  };
  let mut config = json!({"bases":{bid.to_string():{"id":bid,"workspace_id":wid,"name":"神","owner_uuid":uid,"visibility":"private","ready":true}},"workspaces":{wid.to_string():{"visibility":"private"}}});
  let state = json!({"service":{"api_key":"server-secret"},"workspaces":{wid.to_string():{"id":"workspace-kb","ready":true}}});
  let (client, task) = mock(vec![]);
  let list = read(
    &client,
    &session,
    "token",
    "bases",
    &json!({}),
    &state,
    &config,
  )
  .await
  .unwrap();
  assert_eq!(list["bases"].as_array().unwrap().len(), 1);
  assert_eq!(list["bases"][0]["name"], "神");
  assert_eq!(list["bases"][0]["can_manage"], true);
  assert!(!list.to_string().contains("server-secret"));
  let payload = json!({"workspace_id":wid,"base_id":bid,"url":" https://example.test/article ","chunk_size":800,"chunk_overlap":100,"strategy":"heading","kb_id":"forged","user_id":"forged"});
  session.workspaces[0]["role"] = json!("Guest");
  assert_eq!(
    library::status(
      &read(
        &client,
        &session,
        "t",
        "import_url",
        &payload,
        &state,
        &config
      )
      .await
      .unwrap_err()
    ),
    403
  );
  session.workspaces[0]["role"] = json!("Member");
  config["bases"][bid.to_string()]["owner_uuid"] = json!(Uuid::new_v4());
  assert_eq!(
    library::status(
      &read(
        &client,
        &session,
        "t",
        "documents",
        &payload,
        &state,
        &config
      )
      .await
      .unwrap_err()
    ),
    403
  );
  task.join().unwrap();

  let base = json!({"data":{"chunking_config":{"chunk_size":640,"chunk_overlap":80,"strategy":"auto","separators":["\n\n"],"enable_multimodal":true}}});
  let (client, task, requests) = mock_recorded(vec![
    (
      format!("GET /api/v1/knowledge-bases/{bid} "),
      200,
      base.clone(),
    ),
    (
      format!("GET /api/v1/knowledge-bases/{bid}/tags?page=1&page_size=100 "),
      200,
      json!({"data":{"data":[],"total":0}}),
    ),
    (
      format!("GET /api/v1/knowledge-bases/{bid} "),
      200,
      base.clone(),
    ),
    (
      format!("POST /api/v1/knowledge-bases/{bid}/knowledge/url "),
      200,
      json!({"data":{"id":"web-document"}}),
    ),
    (
      format!("GET /api/v1/knowledge-bases/{bid} "),
      200,
      base.clone(),
    ),
    (
      format!("POST /api/v1/knowledge-bases/{bid}/knowledge/url "),
      409,
      json!({"message":"private upstream body"}),
    ),
    (
      format!("GET /api/v1/datasource?kb_id={bid} "),
      200,
      json!([
      {"knowledge_base_id":bid,"id":"connector","name":"飞书","type":"feishu","status":"ready","api_key":"never-return","settings":{"secret":"never-return"}},
      {"knowledge_base_id":"foreign","id":"foreign-secret"}]),
    ),
  ]);
  config["bases"][bid.to_string()]["owner_uuid"] = json!(uid);
  let options = read(
    &client,
    &session,
    "t",
    "web_import_options",
    &payload,
    &state,
    &config,
  )
  .await
  .unwrap();
  assert_eq!(
    options["process_config"]["chunking_config"]["chunk_size"],
    640
  );
  assert_eq!(options["version"], 2);
  assert_eq!(options["tags"], json!([]));
  assert_eq!(
    read(
      &client,
      &session,
      "t",
      "import_url",
      &payload,
      &state,
      &config
    )
    .await
    .unwrap()["document_id"],
    "web-document"
  );
  assert_eq!(
    library::status(
      &read(
        &client,
        &session,
        "t",
        "import_url",
        &payload,
        &state,
        &config
      )
      .await
      .unwrap_err()
    ),
    409
  );
  let sources = read(&client, &session, "t", "sources", &payload, &state, &config)
    .await
    .unwrap();
  assert_eq!(sources["sources"].as_array().unwrap().len(), 1);
  assert!(!sources.to_string().contains("never-return"));
  assert!(!sources.to_string().contains("foreign-secret"));
  task.join().unwrap();
  let requests = requests.lock().unwrap();
  assert_eq!(
    requests[3].1,
    json!({"url":"https://example.test/article","process_config":{"chunking_config":{
    "chunk_size":800,"chunk_overlap":100,"strategy":"heading","separators":["\n\n"],"enable_multimodal":true}}})
  );
  assert!(requests[3]
    .0
    .to_ascii_lowercase()
    .contains("x-api-key: server-secret"));
  drop(requests);
  for invalid in [
    "file:///etc/passwd",
    "https://user:pass@example.test/",
    "https://example.test/a b",
    "https://example.test/\nsecret",
  ] {
    let (client, task) = mock(vec![(
      format!("GET /api/v1/knowledge-bases/{bid} "),
      200,
      base.clone(),
    )]);
    let mut payload = payload.clone();
    payload["url"] = json!(invalid);
    assert_eq!(
      library::status(
        &read(
          &client,
          &session,
          "t",
          "import_url",
          &payload,
          &state,
          &config
        )
        .await
        .unwrap_err()
      ),
      400
    );
    task.join().unwrap();
  }
}

#[tokio::test]
async fn embedding_and_wiki_boundaries_fail_closed() {
  let proxy = json!({"token":"workspace-secret","name":"local-model","parameters":{"dimension":2,"truncate_prompt_tokens":2048,"supports_dimension_override":true}});
  let payload =
    json!({"input":"text","model":"local-model","user_id":"forged","base_url":"http://evil"});
  let request = embedding::prepare(&proxy, "Bearer workspace-secret", &payload).unwrap();
  assert_eq!(
    request,
    json!({"model":"local-model","input":["text"],"truncate":true,"options":{"num_ctx":2048},"dimensions":2})
  );
  assert_eq!(
    library::status(&embedding::prepare(&proxy, "Bearer user-login", &payload).unwrap_err()),
    401
  );
  assert_eq!(
    library::status(
      &embedding::prepare(&Value::Null, "Bearer workspace-secret", &payload).unwrap_err()
    ),
    401
  );
  for input in [json!([1]), json!([]), json!(true)] {
    let mut p = payload.clone();
    p["input"] = input;
    assert_eq!(
      library::status(&embedding::prepare(&proxy, "Bearer workspace-secret", &p).unwrap_err()),
      400
    );
  }
  let (mut client, task) = mock(vec![(
    "POST /api/embed ".into(),
    200,
    json!({"embeddings":[[0.5,0.25]],"prompt_eval_count":7}),
  )]);
  client.ollama = client.cloud.clone();
  let response = embedding::infer(&client, &request).await.unwrap();
  assert_eq!(embedding::tokens(&response).unwrap(), 7);
  assert_eq!(
    embedding::response(&proxy, &request, &response, 7).unwrap()["usage"]["total_tokens"],
    7
  );
  for tokens in [json!(true), json!(-1), Value::Null, json!(u64::MAX)] {
    assert!(embedding::tokens(&json!({"prompt_eval_count":tokens})).is_err());
  }
  assert!(embedding::response(&proxy, &request, &json!({"embeddings":[[0.5]]}), 7).is_err());
  task.join().unwrap();
  let visible = HashSet::from(["own".into()]);
  assert!(library::wiki_readable(
    &json!({"knowledge_base_id":"kb","source_refs":["own|chunk"]}),
    "kb",
    &visible,
    false
  ));
  assert!(!library::wiki_readable(
    &json!({"knowledge_base_id":"kb","source_refs":["hidden|chunk"]}),
    "kb",
    &visible,
    true
  ));
  assert!(!library::wiki_readable(
    &json!({"knowledge_base_id":"kb"}),
    "kb",
    &visible,
    false
  ));
  assert!(!library::wiki_readable(
    &json!({"knowledge_base_id":"foreign","source_refs":["own|chunk"]}),
    "kb",
    &visible,
    true
  ));
  let uid = Uuid::new_v4().to_string();
  let channel = format!("pn:{}", "c".repeat(40));
  let entry = json!({"knowledge_base_id":"kb","title":"quick","channel":channel,"metadata":{"content":format!("来源：小马笔记\n\n目录：随手记\n\n所属用户：{uid}\n\n正文")}});
  assert!(library::entry_source(&entry, "kb", "wid", &uid, &visible).is_some());
  assert!(library::entry_source(&entry, "kb", "wid", "foreign", &visible).is_none());
}

/// Uses only an explicitly named disposable database. Run with --ignored --test-threads=1.
#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL ending in /weknora_native_test"]
async fn native_database_create_recovery_concurrent_settings_and_workspace_sync() {
  let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL");
  assert!(url.ends_with("/weknora_native_test"));
  let pool = sqlx::postgres::PgPoolOptions::new()
    .max_connections(6)
    .connect(&url)
    .await
    .unwrap();
  sqlx::raw_sql("CREATE SCHEMA IF NOT EXISTS auth;
    CREATE TABLE IF NOT EXISTS auth.users(id uuid PRIMARY KEY,deleted_at timestamptz,banned_until timestamptz);
    CREATE TABLE IF NOT EXISTS af_user(uid bigint PRIMARY KEY,uuid uuid,email text);
    CREATE TABLE IF NOT EXISTS af_workspace(workspace_id uuid PRIMARY KEY,workspace_name text,owner_uid bigint,deleted_at timestamptz,is_initialized bool);
    CREATE TABLE IF NOT EXISTS af_workspace_member(workspace_id uuid,uid bigint,role_id int);
    TRUNCATE auth.users,af_user,af_workspace,af_workspace_member;")
    .execute(&pool).await.unwrap();
  sqlx::raw_sql(include_str!(
    "../../../migrations/20261106020000_weknora_sync_state.sql"
  ))
  .execute(&pool)
  .await
  .unwrap();
  sqlx::raw_sql("TRUNCATE af_weknora_sync_state")
    .execute(&pool)
    .await
    .unwrap();
  sqlx::raw_sql(include_str!(
    "../../../migrations/20261112010000_weknora_library_settings.sql"
  ))
  .execute(&pool)
  .await
  .unwrap();
  let uid = Uuid::new_v4();
  let member = Uuid::new_v4();
  let wid = Uuid::new_v4();
  let request_id = Uuid::new_v4();
  let (client, task) = mock(vec![]);
  assert!(sync::run_once(&pool, &client).await.is_err()); // No writes before legacy import.
  task.join().unwrap();
  let bid = Uuid::new_v5(
    &Uuid::NAMESPACE_URL,
    format!("ponynotes:base:{uid}:{wid}:{request_id}").as_bytes(),
  );
  let proxy = json!({"token":"workspace-secret","name":"local-model","source_id":"original-embedding","id":"workspace-embedding","ready":true,
    "parameters":{"dimension":2,"truncate_prompt_tokens":2048}});
  let state = json!({"native_library_version":0,"users":{"old-account":{"workspaces":{"old":"untouched"}}},"service":{"api_key":"service-key","tenant_id":1},
    "workspaces":{wid.to_string():{"id":"workspace-kb","ready":true,"notes":{},"embedding_proxy":proxy}}});
  sqlx::query("UPDATE af_weknora_sync_state SET state=$1,settings='{}'::jsonb WHERE singleton")
    .bind(&state)
    .execute(&pool)
    .await
    .unwrap();
  let session = library::Session {
    uid,
    user_id: 1,
    workspaces: vec![
      json!({"workspace_id":wid,"workspace_name":"空间","owner_uid":1,"role":"Owner"}),
    ],
  };
  let payload =
    json!({"workspace_id":wid,"request_id":request_id,"name":"神","visibility":"private"});
  let (client, task) = mock(vec![
    (
      format!("GET /api/v1/knowledge-bases/{bid} "),
      404,
      json!({}),
    ),
    ("POST /api/v1/knowledge-bases ".into(), 500, json!({})),
    (
      format!("GET /api/v1/knowledge-bases/{bid} "),
      200,
      json!({"data":{"id":bid,"embedding_model_id":"workspace-embedding"}}),
    ),
  ]);
  assert!(
    library::execute(&pool, &client, &session, "t", "create", &payload)
      .await
      .is_err()
  );
  let config: Value =
    sqlx::query_scalar("SELECT settings FROM af_weknora_sync_state WHERE singleton")
      .fetch_one(&pool)
      .await
      .unwrap();
  assert_eq!(config["bases"][bid.to_string()]["ready"], false);
  assert_eq!(
    config["bases"][bid.to_string()]["owner_uuid"],
    uid.to_string()
  );
  for _ in 0..2 {
    assert_eq!(
      library::execute(&pool, &client, &session, "t", "create", &payload)
        .await
        .unwrap()["base"]["id"],
      bid.to_string()
    );
  }
  task.join().unwrap();
  let (client, task) = mock(vec![]);
  let favorite = json!({"workspace_id":wid,"base_id":bid,"is_favorite":true});
  let visibility = json!({"workspace_id":wid,"base_id":bid,"visibility":"shared"});
  let (fav, vis) = tokio::join!(
    library::execute(&pool, &client, &session, "t", "favorite", &favorite),
    library::execute(&pool, &client, &session, "t", "visibility", &visibility)
  );
  fav.unwrap();
  vis.unwrap();
  // Worker writes cannot clobber independently locked library settings.
  sync::persist(
    &mut pool.acquire().await.unwrap(),
    "service",
    None,
    &json!({"tenant_id":1,"api_key":"service-key"}),
  )
  .await
  .unwrap();
  let list = library::execute(&pool, &client, &session, "t", "bases", &json!({}))
    .await
    .unwrap();
  let base = list["bases"]
    .as_array()
    .unwrap()
    .iter()
    .find(|b| b["id"] == bid.to_string())
    .unwrap();
  assert_eq!(base["is_favorite"], true);
  assert_eq!(base["visibility"], "shared");
  task.join().unwrap();
  sqlx::query("INSERT INTO af_user VALUES(1,$1,'owner@test'),(2,$2,'member@test')")
    .bind(uid)
    .bind(member)
    .execute(&pool)
    .await
    .unwrap();
  sqlx::query("INSERT INTO auth.users(id) VALUES($1),($2)")
    .bind(uid)
    .bind(member)
    .execute(&pool)
    .await
    .unwrap();
  sqlx::query("INSERT INTO af_workspace VALUES($1,'空间',1,NULL,true)")
    .bind(wid)
    .execute(&pool)
    .await
    .unwrap();
  sqlx::query("INSERT INTO af_workspace_member VALUES($1,1,1),($1,2,2)")
    .bind(wid)
    .execute(&pool)
    .await
    .unwrap();
  let captured_owner = embedding::owner(&pool, wid).await.unwrap();
  sqlx::query("UPDATE af_workspace SET owner_uid=2 WHERE workspace_id=$1")
    .bind(wid)
    .execute(&pool)
    .await
    .unwrap();
  assert_eq!(captured_owner, 1);
  assert_eq!(embedding::owner(&pool, wid).await.unwrap(), 2);
  sqlx::query("UPDATE auth.users SET banned_until=now()+interval '1 hour' WHERE id=$1")
    .bind(member)
    .execute(&pool)
    .await
    .unwrap();
  assert_eq!(
    library::status(&embedding::owner(&pool, wid).await.unwrap_err()),
    403
  );
  sqlx::query("UPDATE auth.users SET banned_until=NULL WHERE id=$1")
    .bind(member)
    .execute(&pool)
    .await
    .unwrap();
  sqlx::query("UPDATE af_workspace SET deleted_at=now() WHERE workspace_id=$1")
    .bind(wid)
    .execute(&pool)
    .await
    .unwrap();
  assert_eq!(
    library::status(&embedding::owner(&pool, wid).await.unwrap_err()),
    403
  );
  sqlx::query("UPDATE af_workspace SET owner_uid=1,deleted_at=NULL WHERE workspace_id=$1")
    .bind(wid)
    .execute(&pool)
    .await
    .unwrap();
  let admin_path =
    std::env::temp_dir().join(format!("weknora-native-test-{}.json", Uuid::new_v4()));
  std::fs::write(&admin_path, "{}").unwrap();
  std::env::set_var("PONYNOTES_WEKNORA_ADMIN_FILE", &admin_path);
  std::env::set_var("APPFLOWY_GOTRUE_JWT_SECRET", "native-test-secret");
  std::env::set_var(
    "PONYNOTES_WEKNORA_EMBEDDING_URL",
    "http://appflowy_cloud:8000/internal/knowledge",
  );
  let remote = |url: &str| {
    json!({"id":"workspace-embedding","type":"Embedding","source":"remote","name":"local-model",
    "description":format!("PonyNotes workspace embedding {wid}"),"parameters":{"base_url":url,"embedding_parameters":proxy["parameters"]}})
  };
  let legacy_url = format!("http://old-python:18083/internal/knowledge/{wid}/v1");
  let native_url = format!("http://appflowy_cloud:8000/internal/knowledge/{wid}/v1");
  let provision = || {
    vec![
      (
        "POST /api/v1/auth/login ".into(),
        200,
        json!({"token":"admin","active_tenant":{"id":1}}),
      ),
      (
        "GET /api/v1/tenants/1/api-keys ".into(),
        200,
        json!({"data":[{"id":1,"name":"PonyNotes workspaces","capabilities":["retrieve","ingest","manage_kbs","manage_datasources"]}]}),
      ),
      (
        "GET /api/v1/models ".into(),
        200,
        json!({"data":[remote(&legacy_url)]}),
      ),
      (
        "GET /api/v1/system/parser-engines ".into(),
        200,
        json!({"data":[]}),
      ),
    ]
  };
  let scan = |number| {
    vec![
      (
        format!("GET /api/workspace/{wid}/folder?"),
        200,
        json!({"data":{"view_id":wid,"name":"root","children":[],"is_space":true}}),
      ),
      (
        format!("GET /api/workspace/{wid}/database "),
        200,
        json!({"data":[]}),
      ),
      (
        format!("GET /api/workspace/{wid}/quick-note?"),
        200,
        json!({"data":{"quick_notes":[{"id":format!("quick-{number}"),"created_at":"2026-11-12","data":[{"insert":"note"}]}],"has_more":false}}),
      ),
    ]
  };
  let mut responses = provision();
  responses.extend(scan(1));
  responses.extend(scan(2));
  responses.extend(vec![
    (
      "GET /api/v1/knowledge-bases/workspace-kb ".into(),
      200,
      json!({"data":{"id":"workspace-kb","embedding_model_id":"workspace-embedding"}}),
    ),
    (
      "GET /api/v1/models ".into(),
      200,
      json!({"data":[remote(&legacy_url)]}),
    ),
    (
      "PUT /api/v1/models/workspace-embedding ".into(),
      200,
      json!({"data":remote(&native_url)}),
    ),
    (
      "GET /api/v1/knowledge-bases/workspace-kb/knowledge?".into(),
      200,
      json!({"data":[],"total":0}),
    ),
    (
      "POST /api/v1/knowledge-bases/workspace-kb/knowledge/manual ".into(),
      200,
      json!({}),
    ),
    (
      "POST /api/v1/knowledge-bases/workspace-kb/knowledge/manual ".into(),
      200,
      json!({}),
    ),
  ]);
  let (client, task, requests) = mock_recorded(responses);
  let report = sync::run_once(&pool, &client).await.unwrap();
  assert_eq!(report["errors"], json!([]));
  assert_eq!(report["workspaces"][0]["parts"], 2);
  task.join().unwrap();
  let requests = requests.lock().unwrap();
  let exports = requests
    .iter()
    .filter(|(h, _)| h.starts_with("POST /api/v1/knowledge-bases/workspace-kb/knowledge/manual"))
    .map(|(_, b)| b["content"].as_str().unwrap().to_owned())
    .collect::<Vec<_>>();
  assert!(exports
    .iter()
    .any(|s| s.contains(&format!("所属用户：{uid}"))));
  assert!(exports
    .iter()
    .any(|s| s.contains(&format!("所属用户：{member}"))));
  let update = requests
    .iter()
    .find(|(h, _)| h.starts_with("PUT /api/v1/models/"))
    .unwrap();
  assert_eq!(update.1["parameters"]["base_url"], native_url);
  drop(requests);
  let saved: Value = sqlx::query_scalar("SELECT state FROM af_weknora_sync_state WHERE singleton")
    .fetch_one(&pool)
    .await
    .unwrap();
  assert_eq!(saved["users"], state["users"]);
  let mut responses = provision();
  responses.extend(scan(1));
  responses.push((format!("GET /api/workspace/{wid}/folder?"), 503, json!({})));
  let (client, task) = mock(responses);
  let report = sync::run_once(&pool, &client).await.unwrap();
  assert_eq!(report["errors"].as_array().unwrap().len(), 1);
  task.join().unwrap();
  let after: Value =
    sqlx::query_scalar("SELECT state->'workspaces' FROM af_weknora_sync_state WHERE singleton")
      .fetch_one(&pool)
      .await
      .unwrap();
  assert_eq!(after, saved["workspaces"]); // Failed member scan never prunes or changes prior knowledge.
  let mut connection = pool.acquire().await.unwrap();
  sqlx::query("SELECT pg_advisory_lock(781142063)")
    .execute(&mut *connection)
    .await
    .unwrap();
  let (client, task) = mock(vec![]);
  assert_eq!(
    sync::run_once(&pool, &client).await.unwrap()["skipped"],
    "already_running"
  );
  task.join().unwrap();
  sqlx::query("SELECT pg_advisory_unlock(781142063)")
    .execute(&mut *connection)
    .await
    .unwrap();
  std::fs::remove_file(admin_path).unwrap();
  pool.close().await;
}
