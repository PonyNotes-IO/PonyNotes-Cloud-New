#![cfg(not(feature = "sync-v2"))]

use client_api::entity::QueryCollabParams;
use client_api_test::{TestClient, User};
use collab_document::{blocks::Block, document::DocumentBody};
use collab_entity::CollabType;
use serde_json::json;
use shared_entity::dto::workspace_dto::{CreatePageParams, ViewLayout};
use std::time::{Duration, Instant};
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires a disposable test-server account with document history enabled"]
async fn desktop_websocket_edit_creates_automatic_history() -> anyhow::Result<()> {
  let mut client = TestClient::new(
    User {
      email: std::env::var("HISTORY_SMOKE_EMAIL")?,
      password: std::env::var("HISTORY_SMOKE_PASSWORD")?,
    },
    true,
  )
  .await;
  let workspace = client.api_client.get_workspaces().await?[0].workspace_id;
  let folder = client
    .api_client
    .get_workspace_folder(&workspace, Some(2), None)
    .await?;
  let parent = folder
    .children
    .iter()
    .find(|view| view.is_space)
    .ok_or_else(|| anyhow::anyhow!("missing test space"))?;
  let document = Uuid::new_v4();
  let marker = format!("history-v1-{document}");
  client
    .api_client
    .create_workspace_page_view(
      workspace,
      &CreatePageParams {
        parent_view_id: parent.view_id,
        layout: ViewLayout::Document,
        name: Some(marker.clone()),
        page_data: None,
        view_id: Some(document),
        collab_id: Some(document),
      },
    )
    .await?;
  let state = client
    .api_client
    .get_collab(QueryCollabParams::new(
      document,
      CollabType::Document,
      workspace,
    ))
    .await?;
  client
    .open_collab_with_doc_state(
      workspace,
      document,
      CollabType::Document,
      state.encode_collab.doc_state.to_vec(),
    )
    .await;
  let history = client
    .api_client
    .list_document_versions(&workspace, &document, 0)
    .await?;
  assert!(history.items.is_empty());
  let started = Instant::now();
  {
    let mut collab = client.collabs[&document].collab.write().await;
    let body =
      DocumentBody::from_collab(&collab).ok_or_else(|| anyhow::anyhow!("missing document"))?;
    let parent = body.get_document_data(&collab.transact())?.page_id;
    body.insert_block(
      &mut collab.transact_mut(),
      Block {
        id: Uuid::new_v4().to_string(),
        ty: "paragraph".into(),
        parent,
        children: Uuid::new_v4().to_string(),
        external_id: None,
        external_type: None,
        data: [("delta".into(), json!([{"insert": marker}]))].into(),
      },
      None,
    )?;
  }
  loop {
    let history = client
      .api_client
      .list_document_versions(&workspace, &document, 0)
      .await?;
    if let Some(version) = history.items.first() {
      assert_eq!(history.items.len(), 1);
      assert_eq!(version.source, "automatic");
      assert!(started.elapsed() >= Duration::from_secs(115));
      let payload = client
        .api_client
        .get_document_version(&workspace, &document, &version.snapshot_id.parse()?)
        .await?;
      assert!(payload.document.to_string().contains(&marker));
      println!(
        "desktop WebSocket edit checkpoint after {} seconds",
        started.elapsed().as_secs()
      );
      return Ok(());
    }
    anyhow::ensure!(
      started.elapsed() < Duration::from_secs(220),
      "missing automatic history"
    );
    tokio::time::sleep(Duration::from_secs(5)).await;
  }
}
