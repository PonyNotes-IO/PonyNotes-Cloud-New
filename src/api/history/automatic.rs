use super::{version_target, write_version};
use crate::{biz::collab::folder_view::check_if_view_is_space, state::AppState};
use app_error::AppError;
use appflowy_collaborate::ws2::WorkspaceCollabInstanceCache;
use appflowy_proto::Rid;
use collab::entity::EncodedCollab;
use collab::{
  core::collab::{default_client_id, CollabOptions},
  core::origin::CollabOrigin,
  preclude::Collab,
};
use collab_document::document::Document;
use collab_folder::{Folder, View, ViewLayout};
use sqlx::{Postgres, Transaction};
use std::time::Duration;
use uuid::Uuid;

fn ordinary_document(view: &View) -> bool {
  view.layout == ViewLayout::Document
    && !check_if_view_is_space(view)
    && view
      .extra
      .as_deref()
      .and_then(|extra| serde_json::from_str::<serde_json::Value>(extra).ok())
      .is_none_or(|extra| {
        extra.get("view_type").and_then(|value| value.as_str()) != Some("handwriting_saber")
      })
}

pub(super) fn active_document_path(
  folder: &Folder,
  workspace: Uuid,
  oid: Uuid,
  uid: i64,
  trash: &std::collections::HashSet<String>,
) -> Option<Vec<std::sync::Arc<View>>> {
  let mut id = oid.to_string();
  let mut path = Vec::new();
  let workspace = workspace.to_string();
  while id != workspace {
    if path.len() >= 128 || trash.contains(&id) {
      return None;
    }
    let node = folder.get_view(&id, uid)?;
    id = node.parent_view_id.clone();
    path.push(node);
  }
  path.first().filter(|view| ordinary_document(view))?;
  Some(path)
}

pub async fn run_automatic_versions(state: AppState) {
  let mut ticks = tokio::time::interval(Duration::from_secs(5));
  ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
  loop {
    ticks.tick().await;
    if let Err(err) = process_due(&state).await {
      tracing::warn!(%err, "could not poll automatic document versions");
    }
  }
}

#[derive(sqlx::FromRow)]
struct Pending {
  workspace_id: Uuid,
  oid: Uuid,
  first_edit_ms: i64,
  last_edit_ms: i64,
  last_edit_seq: i32,
}

enum Checkpoint {
  Covered(Rid),
  Discard,
  Wait,
}

pub(super) async fn mark_covered(
  tx: &mut Transaction<'_, Postgres>,
  workspace: Uuid,
  oid: Uuid,
  rid: Rid,
) -> Result<(), AppError> {
  sqlx::query(
    "UPDATE af_document_version_pending
     SET covered_edit_ms=$3, covered_edit_seq=$4, retry_at='-infinity', storage_limited=FALSE,
         first_edit_ms=CASE WHEN (last_edit_ms, last_edit_seq) > ($3,$4)
           THEN GREATEST(first_edit_ms, $3) END,
         last_edit_ms=CASE WHEN (last_edit_ms, last_edit_seq) > ($3,$4) THEN last_edit_ms END
     WHERE workspace_id=$1 AND oid=$2 AND (covered_edit_ms, covered_edit_seq) < ($3,$4)",
  )
  .bind(workspace)
  .bind(oid)
  .bind(rid.timestamp as i64)
  .bind(i32::from(rid.seq_no))
  .execute(&mut **tx)
  .await?;
  Ok(())
}

async fn process_due(state: &AppState) -> Result<(), AppError> {
  for _ in 0..20 {
    let mut tx = state.pg_pool.begin().await?;
    let pending = sqlx::query_as::<_, Pending>(
      "SELECT workspace_id, oid, first_edit_ms, last_edit_ms, last_edit_seq
       FROM af_document_version_pending
       WHERE due_ms <= (EXTRACT(EPOCH FROM NOW()) * 1000)::bigint AND retry_at <= NOW()
       ORDER BY due_ms LIMIT 1 FOR UPDATE SKIP LOCKED",
    )
    .fetch_optional(&mut *tx)
    .await?;
    let Some(pending) = pending else {
      return Ok(());
    };
    // Keep the row lock while rolling back any failed attempt before scheduling a retry.
    sqlx::query("SAVEPOINT automatic_document_version")
      .execute(&mut *tx)
      .await?;
    let attempt = tokio::time::timeout(
      Duration::from_secs(20),
      checkpoint(state, &mut tx, &pending),
    )
    .await;
    let (retry_seconds, storage_limited) = match attempt {
      Ok(Ok(Checkpoint::Covered(rid))) => {
        mark_covered(&mut tx, pending.workspace_id, pending.oid, rid).await?;
        tx.commit().await?;
        continue;
      },
      Ok(Ok(Checkpoint::Discard))
      | Ok(Err(AppError::InvalidRequest(_) | AppError::RecordDeleted(_))) => {
        sqlx::query("DELETE FROM af_document_version_pending WHERE workspace_id=$1 AND oid=$2")
          .bind(pending.workspace_id)
          .bind(pending.oid)
          .execute(&mut *tx)
          .await?;
        tx.commit().await?;
        continue;
      },
      Ok(Ok(Checkpoint::Wait)) => (10.0_f64, false),
      Ok(Err(AppError::PlanLimitExceeded(_))) => (3600.0, true),
      Ok(Err(err)) => {
        tracing::warn!(%err, document=%pending.oid, "automatic document version will retry");
        (60.0, false)
      },
      Err(_) => (60.0, false),
    };
    sqlx::query("ROLLBACK TO SAVEPOINT automatic_document_version")
      .execute(&mut *tx)
      .await?;
    sqlx::query(
      "UPDATE af_document_version_pending
       SET retry_at=clock_timestamp()+make_interval(secs=>$3), storage_limited=$4
       WHERE workspace_id=$1 AND oid=$2",
    )
    .bind(pending.workspace_id)
    .bind(pending.oid)
    .bind(retry_seconds)
    .bind(storage_limited)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
  }
  Ok(())
}

async fn checkpoint(
  state: &AppState,
  tx: &mut Transaction<'_, Postgres>,
  pending: &Pending,
) -> Result<Checkpoint, AppError> {
  let workspace = pending.workspace_id;
  let oid = pending.oid;
  let Some(target) = version_target(state, workspace, oid).await? else {
    return Ok(Checkpoint::Discard);
  };
  if target.days <= 0 {
    return Ok(Checkpoint::Discard);
  }
  let folder = state.ws_server.get_folder(workspace).await?;
  let trash = folder
    .get_all_trash_sections(target.owner)
    .into_iter()
    .map(|entry| entry.id)
    .collect();
  if active_document_path(&folder, workspace, oid, target.owner, &trash).is_none() {
    return Ok(Checkpoint::Discard);
  }

  let (rid, encoded) = state
    .collab_cache
    .get_document_history_state(&workspace, oid)
    .await?;
  if rid.timestamp < pending.last_edit_ms as u64 {
    return Ok(Checkpoint::Wait);
  }
  let now = chrono::Utc::now().timestamp_millis();
  if !is_due(now, pending.first_edit_ms, rid.timestamp as i64) {
    return Ok(Checkpoint::Wait);
  }
  let document = decode_document(oid, encoded).await?;
  write_version(state, tx, &target, None, document, "automatic").await?;
  // Persisted collab states retain milliseconds, but omit the Redis sequence.
  let seq_no = if rid.timestamp == pending.last_edit_ms as u64 {
    rid.seq_no.max(pending.last_edit_seq as u16)
  } else {
    rid.seq_no
  };
  Ok(Checkpoint::Covered(Rid::new(rid.timestamp, seq_no)))
}

pub(super) async fn decode_document(
  oid: Uuid,
  encoded: EncodedCollab,
) -> Result<collab_document::blocks::DocumentData, AppError> {
  tokio::task::spawn_blocking(move || {
    let options =
      CollabOptions::new(oid.to_string(), default_client_id()).with_data_source(encoded.into());
    let collab = Collab::new_with_options(CollabOrigin::Server, options)
      .map_err(|err| AppError::InvalidRequest(err.to_string()))?;
    Document::open(collab)
      .and_then(|doc| doc.get_document_data())
      .map_err(|err| AppError::InvalidRequest(err.to_string()))
  })
  .await
  .map_err(|err| AppError::Internal(err.into()))?
}

fn is_due(now: i64, first: i64, last: i64) -> bool {
  now >= first + 600_000 || now >= last + 120_000
}

#[cfg(test)]
mod tests {
  use super::is_due;

  #[test]
  fn snapshots_at_idle_or_continuous_edit_deadline() {
    assert!(!is_due(179_999, 0, 60_000));
    assert!(is_due(180_000, 0, 60_000));
    assert!(!is_due(599_999, 0, 595_000));
    assert!(is_due(600_000, 0, 595_000));
    assert!(is_due(600_000, 0, 480_000));
    assert!(!is_due(600_000, 500_000, 599_000));
  }
}
