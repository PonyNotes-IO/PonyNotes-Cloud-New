use super::*;
use crate::notes::{document_markdown, note_parts, quick_markdown};
use sha2::Digest;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

/// Tiny local HTTP server; exercises real requests without credentials or external services.
fn mock(responses: Vec<(String, u16, Value)>) -> (Client, thread::JoinHandle<()>) {
  let listener = TcpListener::bind("127.0.0.1:0").unwrap();
  let address = format!("http://{}", listener.local_addr().unwrap());
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
      let body = payload.to_string();
      write!(stream,"HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
    }
  });
  (
    Client::new(address.clone() + "/api/v1/", address).unwrap(),
    task,
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
      "GET /api/workspace ".into(),
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
  let account = json!({"api_key":"only-user-a","workspaces":{wid.to_string():{"id":"kb-a"}}});
  let result = search(&client, Some(&account), uid, "session", wid, "query")
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
  assert!(client.scan("token", &wid.to_string()).await.is_err());
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
  let notes = client.scan("token", &wid.to_string()).await.unwrap();
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
  assert!(search(&client, None, uid, "token", wid, "query")
    .await
    .is_err());
  task.join().unwrap();
  let (client, task) = mock(vec![
    ("GET /gotrue/user".into(), 200, json!({"id":uid})),
    (
      "GET /api/workspace ".into(),
      200,
      json!({"data":[{"workspace_id":Uuid::new_v4()}]}),
    ),
  ]);
  assert!(search(
    &client,
    Some(&json!({"api_key":"foreign"})),
    uid,
    "token",
    wid,
    "query"
  )
  .await
  .is_err());
  task.join().unwrap();
}
