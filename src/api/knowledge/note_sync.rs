use crate::biz::collab::utils::get_latest_collab_document;
use crate::biz::workspace::page_view::{
  is_private_root_page, store_document_page, update_page_collab_data,
};
use crate::state::AppState;
use access_control::act::Action;
use actix_web::web;
use anyhow::{ensure, Result};
use appflowy_collaborate::ws2::WorkspaceCollabInstanceCache;
use collab::core::collab::default_client_id;
use collab::preclude::Map;
use collab::util::MapExt;
use collab_document::{
  blocks::{BlockType, DocumentData},
  document::Document,
  importer::md_importer::MDImporter,
};
use collab_entity::CollabType;
use collab_rt_entity::user::RealtimeUser;
use database::collab::GetCollabOrigin;
use database_entity::dto::CollabParams;
use ponynotes_weknora::{
  library::uuid,
  note_source::{FeishuNote, FeishuNoteSource, WORKSPACE_SYNC_SNAPSHOT},
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::time::Duration;
use uuid::Uuid;

pub(super) async fn status(
  state: &AppState,
  uid: i64,
  workspace: Uuid,
  source: Uuid,
) -> Result<Value> {
  let result: Option<Value> = sqlx::query_scalar(
    "SELECT coalesce(result, CASE WHEN started_at > now() - interval '12 minutes' \
     THEN '{\"status\":\"running\"}'::jsonb ELSE '{\"status\":\"failed\"}'::jsonb END) \
     FROM af_weknora_note_sync WHERE workspace_id = $1 AND uid = $2 AND source_id = $3",
  )
  .bind(workspace)
  .bind(uid)
  .bind(source)
  .fetch_optional(&state.pg_pool)
  .await?;
  let mut result = result.unwrap_or_else(|| json!({"status":"idle"}));
  if let Some(id) = result["view_id"].as_str() {
    let folder = state.ws_server.get_folder(workspace).await?;
    if !is_private_root_page(&folder, uid, workspace, id) {
      result.as_object_mut().unwrap().remove("view_id");
    }
  }
  Ok(result)
}

pub(super) async fn start(
  state: web::Data<AppState>,
  user: RealtimeUser,
  payload: &Value,
  source: FeishuNoteSource,
) -> Result<Value> {
  let workspace = uuid(payload, "workspace_id")?;
  let source_id = uuid(payload, "source_id")?;
  let uid = user.uid;
  let run = Uuid::new_v4();
  let started = sqlx::query(
    "INSERT INTO af_weknora_note_sync (workspace_id, uid, source_id, run_id) VALUES ($1, $2, $3, $4) \
     ON CONFLICT (workspace_id, uid, source_id) DO UPDATE SET run_id = $4, result = NULL, started_at = now() \
     WHERE af_weknora_note_sync.result IS NOT NULL OR af_weknora_note_sync.started_at <= now() - interval '12 minutes'",
  ).bind(workspace).bind(uid).bind(source_id).bind(run).execute(&state.pg_pool).await?;
  if started.rows_affected() > 0 {
    // ponytail: this manual job survives closing the client. Cloud restarts require a retry.
    actix_web::rt::spawn(async move {
      let mut result = match tokio::time::timeout(
        Duration::from_secs(600),
        sync(&state, user, workspace, source_id, source),
      )
      .await
      {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => {
          tracing::warn!(%run, "note sync failed before pages could be saved");
          json!({"status":"failed"})
        },
        Err(_) => {
          tracing::warn!(%run, "note sync timed out");
          json!({"status":"failed"})
        },
      };
      result["finished_at"] = json!(chrono::Utc::now().to_rfc3339());
      if sqlx::query("UPDATE af_weknora_note_sync SET result = $2 WHERE run_id = $1")
        .bind(run)
        .bind(result)
        .execute(&state.pg_pool)
        .await
        .is_err()
      {
        tracing::warn!(%run, "could not save note sync result");
      }
    });
  }
  Ok(json!({"status":"running"}))
}

async fn sync(
  state: &AppState,
  user: RealtimeUser,
  workspace: Uuid,
  source_id: Uuid,
  source: FeishuNoteSource,
) -> Result<Value> {
  source.refresh().await?;
  let notes = source.notes().await?;
  if notes.is_empty() {
    return Ok(json!({"status":"success", "saved":0, "failed":0}));
  }
  // ponytail: bound one manual run; partition large Wiki selections across connections.
  ensure!(notes.len() <= 100, "too many notes in one sync");
  let namespace = Uuid::new_v5(
    &workspace,
    format!("feishu-wiki:{}:{source_id}", user.uid).as_bytes(),
  );
  let mut first_page = None;
  let mut saved = 0;
  let mut unchanged = 0;
  let mut failed = 0;
  let mut notes = notes.into_iter();
  while let Some(note) = notes.next() {
    if source.check_scope().await.is_err() {
      failed += 1 + notes.len();
      break;
    }
    let Ok(markdown) = source.markdown(&note).await else {
      failed += 1;
      continue;
    };
    if source.check_scope().await.is_err() {
      failed += 1 + notes.len();
      break;
    }
    let id = Uuid::new_v5(&namespace, note.external_id.as_bytes());
    match save_note(state, &user, workspace, id, &note, markdown).await {
      Ok(written) => {
        first_page.get_or_insert(id);
        if written {
          saved += 1;
        } else {
          unchanged += 1;
        }
      },
      Err(_) => failed += 1,
    }
  }
  let status = if failed == 0 {
    "success"
  } else if saved + unchanged == 0 {
    "failed"
  } else {
    "partial"
  };
  Ok(
    json!({"status":status, "saved":saved, "unchanged":unchanged, "failed":failed, "view_id":first_page}),
  )
}

async fn save_note(
  state: &AppState,
  user: &RealtimeUser,
  workspace: Uuid,
  id: Uuid,
  note: &FeishuNote,
  markdown: String,
) -> Result<bool> {
  state
    .workspace_access_control
    .enforce_action(&user.uid, &workspace, Action::Write)
    .await?;
  let folder = state.ws_server.get_folder(workspace).await?;
  let id_string = id.to_string();
  ensure!(
    !folder
      .get_all_trash_sections(user.uid)
      .iter()
      .any(|item| item.id == id_string),
    "note is in trash"
  );
  let original = format!(
    "[在飞书打开](https://feishu.cn/wiki/{})\n\n{markdown}",
    note.external_id
  );
  let revision = digest(&original);
  if folder.get_view(&id_string, user.uid).is_some() {
    ensure!(
      is_private_root_page(&folder, user.uid, workspace, &id_string),
      "note left the private sidebar"
    );
    let document = get_latest_collab_document(
      &state.collab_storage,
      GetCollabOrigin::User { uid: user.uid },
      workspace,
      id,
    )
    .await?;
    let Some(update) = refresh_document(document, id, original, revision)? else {
      return Ok(false);
    };
    update_page_collab_data(
      state,
      user.clone(),
      workspace,
      id,
      CollabType::Document,
      update,
    )
    .await?;
  } else {
    // ponytail: keep the native title after creation so sync cannot overwrite a local rename.
    let title: String = note.title.chars().take(255).collect();
    let mut document =
      Document::create(&id.to_string(), parse(id, original)?, default_client_id())?;
    let hash = digest_data(&document.get_document_data()?)?;
    let root = document.data.clone();
    {
      let mut txn = document.transact_mut();
      root.insert(&mut txn, WORKSPACE_SYNC_SNAPSHOT, hash);
      root.insert(&mut txn, "ponynotes_feishu_revision", revision);
    }
    let params = CollabParams {
      object_id: id,
      collab_type: CollabType::Document,
      updated_at: None,
      encoded_collab_v1: document.encode_collab()?.encode_to_bytes()?.into(),
    };
    store_document_page(
      state,
      user.clone(),
      workspace,
      &workspace,
      id,
      Some(&title),
      params,
      true,
    )
    .await?;
  }
  Ok(true)
}

fn refresh_document(
  document: Document,
  id: Uuid,
  markdown: String,
  revision: String,
) -> Result<Option<Vec<u8>>> {
  let (mut collab, mut body) = document.split();
  let root = collab.data.clone();
  let mut txn = collab.transact_mut();
  let expected: String = root.get_with_txn(&txn, WORKSPACE_SYNC_SNAPSHOT).unwrap_or_default();
  ensure!(
    expected == digest_data(&body.get_document_data(&txn)?)?,
    "note was edited locally"
  );
  let previous: String = root
    .get_with_txn(&txn, "ponynotes_feishu_revision")
    .unwrap_or_default();
  if revision == previous {
    return Ok(None);
  }
  body.reset_with_data(&mut txn, Some(parse(id, markdown)?))?;
  let hash = digest_data(&body.get_document_data(&txn)?)?;
  root.insert(&mut txn, WORKSPACE_SYNC_SNAPSHOT, hash);
  root.insert(&mut txn, "ponynotes_feishu_revision", revision);
  Ok(Some(txn.encode_update_v1()))
}
fn digest(content: &str) -> String {
  format!("{:x}", Sha256::digest(content.as_bytes()))
}

fn digest_data(data: &DocumentData) -> Result<String> {
  let mut value = serde_json::to_value(data)?;
  if let Some(text_map) = value["meta"]
    .get_mut("text_map")
    .and_then(Value::as_object_mut)
  {
    // Text deltas are stored as JSON strings. Canonicalize their attributes as well.
    for delta in text_map.values_mut() {
      if let Some(text) = delta.as_str() {
        *delta = serde_json::from_str(text)?;
      }
    }
  }
  value.sort_all_objects();
  Ok(digest(&value.to_string()))
}

fn parse(id: Uuid, markdown: String) -> Result<DocumentData> {
  let mut data = MDImporter::new(None).import(&id.to_string(), markdown)?;
  for block in data
    .blocks
    .values_mut()
    .filter(|block| block.ty == BlockType::Image.to_string())
  {
    // ponytail: inline and remote images are left at the source until blob import is supported.
    block.ty = BlockType::Paragraph.to_string();
    block.data.clear();
  }
  if let Some(text_map) = &mut data.meta.text_map {
    for delta in text_map.values_mut() {
      let mut entries: Vec<Value> = serde_json::from_str(delta)?;
      for entry in &mut entries {
        if let Some(attributes) = entry.get_mut("attributes").and_then(Value::as_object_mut) {
          if attributes.get("href").is_some_and(|link| {
            !link
              .as_str()
              .and_then(|url| reqwest::Url::parse(url).ok())
              .is_some_and(|url| matches!(url.scheme(), "http" | "https" | "mailto"))
          }) {
            attributes.remove("href");
          }
        }
      }
      *delta = serde_json::to_string(&entries)?;
    }
  }
  Ok(data)
}

#[cfg(test)]
mod tests {
  use super::*;
  use collab::preclude::{updates::decoder::Decode, Update};
  use collab_folder::CollabOrigin;

  #[test]
  fn repeated_sync_updates_the_same_collab_and_rejects_local_edits() {
    let id = Uuid::new_v4();
    let original = "# Original\n\n[**Read more**](https://example.com)";
    let mut document = Document::create(
      &id.to_string(),
      parse(id, original.into()).unwrap(),
      default_client_id(),
    )
    .unwrap();
    let hash = digest_data(&document.get_document_data().unwrap()).unwrap();
    let root = document.data.clone();
    {
      let mut txn = document.transact_mut();
      root.insert(&mut txn, WORKSPACE_SYNC_SNAPSHOT, hash);
      root.insert(&mut txn, "ponynotes_feishu_revision", digest(original));
    }
    let snapshot = document.encode_collab().unwrap();
    let open = || {
      Document::open_with_options(
        CollabOrigin::Empty,
        snapshot.clone().into(),
        &id.to_string(),
        default_client_id(),
      )
      .unwrap()
    };
    assert!(open().paragraphs().join(" ").contains("Read more"));
    let (collab, _) = open().split();
    assert!(collab.to_json_value()[WORKSPACE_SYNC_SNAPSHOT].is_string());
    assert!(
      refresh_document(open(), id, original.into(), digest(original))
        .unwrap()
        .is_none()
    );
    let changed = "# Changed\n\n[Unsafe](javascript:alert%281%29)";
    let update = refresh_document(open(), id, changed.into(), digest(changed))
      .unwrap()
      .unwrap();
    let mut replica = open();
    replica
      .transact_mut()
      .apply_update(Update::decode_v1(&update).unwrap())
      .unwrap();
    assert!(replica.paragraphs().join(" ").contains("Changed"));
    assert!(!replica.paragraphs().join(" ").contains("Original"));
    assert!(replica.paragraphs().join(" ").contains("Unsafe"));
    assert!(!replica.to_markdown_text().join(" ").contains("javascript:"));
    assert!(
      refresh_document(replica, id, changed.into(), digest(changed))
        .unwrap()
        .is_none()
    );
    let mut edited = open();
    let first = edited
      .get_document_data()
      .unwrap()
      .blocks
      .values()
      .find(|block| block.parent == id.to_string())
      .unwrap()
      .id
      .clone();
    edited.delete_block(&first).unwrap();
    assert!(refresh_document(edited, id, changed.into(), digest(changed)).is_err());
  }
}
