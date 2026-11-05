use crate::biz::authentication::jwt::UserUuid;
use crate::biz::subscription::ops::get_user_resource_limit_status;
use crate::state::AppState;
use access_control::act::Action;
use actix_web::{web, HttpResponse};
use app_error::AppError;
use collab::core::collab::{default_client_id, CollabOptions};
use collab::core::origin::CollabOrigin;
use collab::preclude::Collab;
use collab_document::{blocks::DocumentData, document::Document};
use futures::TryStreamExt;
use serde::Deserialize;
use shared_entity::dto::history_dto::{
  DocumentVersionData, DocumentVersionList, DocumentVersionMeta,
};
use shared_entity::response::{AppResponse, JsonAppResponse};
use std::collections::HashSet;
use uuid::Uuid;

pub const MAX_VERSION_BYTES: usize = 5 * 1024 * 1024;
const PAGE_SIZE: i64 = 50;

fn validate_document(data: &DocumentData) -> Result<(), AppError> {
  let invalid = || AppError::InvalidRequest("invalid document block tree".into());
  let root = data.blocks.get(&data.page_id).ok_or_else(invalid)?;
  if root.ty != "page" || root.id != data.page_id {
    return Err(invalid());
  }
  let mut visited = HashSet::new();
  let mut pending = vec![(data.page_id.as_str(), 0)];
  while let Some((id, depth)) = pending.pop() {
    if depth > 256 || !visited.insert(id) {
      return Err(invalid());
    }
    let block = data.blocks.get(id).ok_or_else(invalid)?;
    if block.id != id {
      return Err(invalid());
    }
    if let Some(children) = data.meta.children_map.get(&block.children) {
      for child in children {
        let child_block = data.blocks.get(child).ok_or_else(invalid)?;
        if child_block.parent != id {
          return Err(invalid());
        }
        pending.push((child.as_str(), depth + 1));
      }
    }
  }
  Ok(())
}

// Tree order and content matter; block, children-map and external-text IDs do not.
fn version_content(data: &DocumentData) -> Result<serde_json::Value, AppError> {
  fn node(data: &DocumentData, id: &str, depth: usize) -> Result<serde_json::Value, AppError> {
    if depth > 256 {
      return Err(AppError::InvalidRequest(
        "invalid document block tree".into(),
      ));
    }
    let block = data
      .blocks
      .get(id)
      .ok_or_else(|| AppError::InvalidRequest("missing document block".into()))?;
    let mut attributes = serde_json::to_value(&block.data)?;
    if block.external_type.as_deref() == Some("text") {
      if let Some(text) = block
        .external_id
        .as_ref()
        .and_then(|id| data.meta.text_map.as_ref()?.get(id))
      {
        attributes["delta"] = serde_json::from_str(text)?;
      }
    }
    // Adjacent equal-format text runs may have different CRDT segmentation.
    if let Some(delta) = attributes.get_mut("delta").and_then(|v| v.as_array_mut()) {
      let mut normalized: Vec<serde_json::Value> = Vec::new();
      for mut op in std::mem::take(delta) {
        if op
          .get("attributes")
          .is_some_and(|v| v.is_null() || v.as_object().is_some_and(|m| m.is_empty()))
        {
          op.as_object_mut().unwrap().remove("attributes");
        }
        if op.get("insert").and_then(|v| v.as_str()) == Some("") {
          continue;
        }
        if let Some(last) = normalized.last_mut() {
          if last.get("attributes") == op.get("attributes") {
            if let (Some(a), Some(b)) = (
              last.get("insert").and_then(|v| v.as_str()),
              op.get("insert").and_then(|v| v.as_str()),
            ) {
              last["insert"] = serde_json::Value::String(format!("{a}{b}"));
              continue;
            }
          }
        }
        normalized.push(op);
      }
      *delta = normalized;
    }
    let children = data
      .meta
      .children_map
      .get(&block.children)
      .into_iter()
      .flatten()
      .map(|child| node(data, child, depth + 1))
      .collect::<Result<Vec<_>, _>>()?;
    // Non-text external identities can reference separately stored resources.
    let external = match block.external_type.as_deref() {
      None | Some("") | Some("text") => None,
      _ => Some((&block.external_type, &block.external_id)),
    };
    Ok(
      serde_json::json!({"type": block.ty, "data": attributes, "external": external, "children": children}),
    )
  }
  node(data, &data.page_id, 0)
}

async fn find_existing_version(
  tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
  workspace: Uuid,
  oid: Uuid,
  days: i32,
  content: &serde_json::Value,
) -> Result<Option<(Uuid, i64)>, AppError> {
  let mut rows = sqlx::query_as::<_, (Uuid, i64, Vec<u8>)>(
    "SELECT snapshot_id, (EXTRACT(EPOCH FROM created_at)*1000)::BIGINT, document
     FROM af_document_version WHERE workspace_id=$1 AND oid=$2
       AND expires_at>clock_timestamp() AND created_at>=clock_timestamp()-make_interval(days=>$3)
     ORDER BY created_at DESC, snapshot_id DESC",
  )
  .bind(workspace)
  .bind(oid)
  .bind(days)
  .fetch(&mut **tx);
  while let Some((id, created_at, bytes)) = rows.try_next().await? {
    let stored: DocumentData = serde_json::from_slice(&bytes)?;
    if version_content(&stored)? == *content {
      return Ok(Some((id, created_at)));
    }
  }
  Ok(None)
}

#[derive(Default, Deserialize)]
pub struct HistoryQuery {
  #[serde(default)]
  offset: u32,
}

// Match the document's workspace before consulting object-scoped permission caches.
async fn authorize(
  state: &AppState,
  user: &Uuid,
  workspace: Uuid,
  oid: Uuid,
) -> Result<(i64, i64, i32), AppError> {
  let uid = state.user_cache.get_user_uid(user).await?;
  let owner: Option<i64> = sqlx::query_scalar(
    "SELECT w.owner_uid FROM af_workspace w JOIN af_collab c ON c.workspace_id=w.workspace_id
     WHERE w.workspace_id=$1 AND c.oid=$2 AND c.partition_key=0
       AND w.deleted_at IS NULL AND c.deleted_at IS NULL",
  )
  .bind(workspace)
  .bind(oid)
  .fetch_optional(&state.pg_pool)
  .await?;
  let owner = owner.ok_or(AppError::NotEnoughPermissions)?;
  state
    .collab_access_control
    .enforce_action(&workspace, &uid, &oid, Action::Write)
    .await?;
  let days: Option<i32> = sqlx::query_scalar(
    "SELECT p.version_history_days FROM af_user_subscriptions s
     JOIN af_subscription_plans p ON p.id=s.plan_id
     WHERE s.uid=$1 AND s.status='active' AND s.start_date<=NOW() AND s.end_date>NOW()
     ORDER BY s.end_date DESC, s.id DESC LIMIT 1",
  )
  .bind(owner)
  .fetch_optional(&state.pg_pool)
  .await?;
  let days = days.unwrap_or(0);
  if days <= 0 {
    return Err(AppError::PlanLimitExceeded(
      "version_history_unavailable".into(),
    ));
  }
  Ok((uid, owner, days))
}

fn permission_error(error: AppError) -> actix_web::Error {
  let status = if matches!(
    error,
    AppError::NotEnoughPermissions | AppError::PlanLimitExceeded(_)
  ) {
    actix_web::http::StatusCode::FORBIDDEN
  } else if matches!(error, AppError::InvalidRequest(_)) {
    actix_web::http::StatusCode::BAD_REQUEST
  } else if matches!(error, AppError::RecordNotFound(_)) {
    actix_web::http::StatusCode::NOT_FOUND
  } else {
    actix_web::http::StatusCode::INTERNAL_SERVER_ERROR
  };
  actix_web::error::InternalError::from_response(
    "document history",
    HttpResponse::build(status).json(AppResponse::<()>::from(error)),
  )
  .into()
}

pub async fn list(
  user: UserUuid,
  state: web::Data<AppState>,
  path: web::Path<(Uuid, Uuid)>,
  query: web::Query<HistoryQuery>,
) -> actix_web::Result<JsonAppResponse<DocumentVersionList>> {
  let (workspace, oid) = path.into_inner();
  let (_, _, days) = authorize(&state, &user, workspace, oid)
    .await
    .map_err(permission_error)?;
  let rows: Vec<(Uuid, i64)> = sqlx::query_as(
    "SELECT snapshot_id, (EXTRACT(EPOCH FROM created_at)*1000)::BIGINT FROM af_document_version
     WHERE workspace_id=$1 AND oid=$2 AND expires_at>NOW()
       AND created_at>=NOW()-make_interval(days=>$3)
     ORDER BY created_at DESC, snapshot_id DESC LIMIT $4 OFFSET $5",
  )
  .bind(workspace)
  .bind(oid)
  .bind(days)
  .bind(PAGE_SIZE + 1)
  .bind(i64::from(query.offset))
  .fetch_all(&state.pg_pool)
  .await
  .map_err(AppError::from)
  .map_err(permission_error)?;
  let has_more = rows.len() > PAGE_SIZE as usize;
  let items = rows
    .into_iter()
    .take(PAGE_SIZE as usize)
    .map(|(id, created_at)| DocumentVersionMeta {
      snapshot_id: id.to_string(),
      object_id: oid.to_string(),
      created_at,
      reused: false,
    })
    .collect();
  Ok(
    AppResponse::Ok()
      .with_data(DocumentVersionList {
        items,
        has_more,
        retention_days: days,
      })
      .into(),
  )
}

pub async fn get(
  user: UserUuid,
  state: web::Data<AppState>,
  path: web::Path<(Uuid, Uuid, Uuid)>,
) -> actix_web::Result<JsonAppResponse<DocumentVersionData>> {
  let (workspace, oid, id) = path.into_inner();
  let (_, _, days) = authorize(&state, &user, workspace, oid)
    .await
    .map_err(permission_error)?;
  let bytes: Option<Vec<u8>> = sqlx::query_scalar(
    "SELECT document FROM af_document_version WHERE workspace_id=$1 AND oid=$2 AND snapshot_id=$3
     AND expires_at>NOW() AND created_at>=NOW()-make_interval(days=>$4)",
  )
  .bind(workspace)
  .bind(oid)
  .bind(id)
  .bind(days)
  .fetch_optional(&state.pg_pool)
  .await
  .map_err(AppError::from)
  .map_err(permission_error)?;
  let bytes = bytes.ok_or_else(|| {
    permission_error(AppError::RecordNotFound(
      "version expired or missing".into(),
    ))
  })?;
  let document = serde_json::from_slice(&bytes)
    .map_err(AppError::from)
    .map_err(permission_error)?;
  Ok(
    AppResponse::Ok()
      .with_data(DocumentVersionData { document })
      .into(),
  )
}

pub async fn save(
  user: UserUuid,
  state: web::Data<AppState>,
  path: web::Path<(Uuid, Uuid)>,
  body: web::Json<DocumentVersionData>,
) -> actix_web::Result<JsonAppResponse<DocumentVersionMeta>> {
  let (workspace, oid) = path.into_inner();
  let (uid, owner, days) = authorize(&state, &user, workspace, oid)
    .await
    .map_err(permission_error)?;
  let document: DocumentData = serde_json::from_value(body.into_inner().document)
    .map_err(|e| permission_error(AppError::InvalidRequest(e.to_string())))?;
  validate_document(&document).map_err(permission_error)?;
  let bytes = serde_json::to_vec(&document)
    .map_err(AppError::from)
    .map_err(permission_error)?;
  if bytes.len() > MAX_VERSION_BYTES {
    return Err(permission_error(AppError::InvalidRequest(
      "version exceeds 5 MiB".into(),
    )));
  }
  let content = version_content(&document).map_err(permission_error)?;
  // Validate in an isolated document; history requests never open or mutate the live collab.
  tokio::task::spawn_blocking(move || {
    let collab = Collab::new_with_options(
      CollabOrigin::Empty,
      CollabOptions::new(oid.to_string(), default_client_id()),
    )
    .map_err(|e| AppError::InvalidRequest(e.to_string()))?;
    Document::create_with_data(collab, document)
      .map_err(|e| AppError::InvalidRequest(e.to_string()))?;
    Ok::<_, AppError>(())
  })
  .await
  .map_err(|e| permission_error(AppError::Internal(e.into())))?
  .map_err(permission_error)?;
  let resource = get_user_resource_limit_status(&state.pg_pool, owner)
    .await
    .map_err(permission_error)?;
  let limit = (resource.storage_limit_mb * 1024.0 * 1024.0) as i64;
  let mut tx = state
    .pg_pool
    .begin()
    .await
    .map_err(AppError::from)
    .map_err(permission_error)?;
  // Serialize manual saves across all workspaces billed to this owner.
  sqlx::query("SELECT pg_advisory_xact_lock($1)")
    .bind(owner)
    .execute(&mut *tx)
    .await
    .map_err(AppError::from)
    .map_err(permission_error)?;
  if let Some((id, created_at)) = find_existing_version(&mut tx, workspace, oid, days, &content)
    .await
    .map_err(permission_error)?
  {
    tx.commit()
      .await
      .map_err(AppError::from)
      .map_err(permission_error)?;
    return Ok(
      AppResponse::Ok()
        .with_data(DocumentVersionMeta {
          snapshot_id: id.to_string(),
          object_id: oid.to_string(),
          created_at,
          reused: true,
        })
        .into(),
    );
  }
  let used = database::subscription::get_user_total_usage_bytes(&mut *tx, owner)
    .await
    .map_err(permission_error)?;
  if used.saturating_add(bytes.len() as i64) > limit {
    return Err(permission_error(AppError::PlanLimitExceeded(
      "cloud_storage_full".into(),
    )));
  }
  let (id, created_at): (Uuid, i64) = sqlx::query_as(
    "INSERT INTO af_document_version(workspace_id,oid,created_by,document,expires_at)
     VALUES($1,$2,$3,$4,NOW()+make_interval(days=>$5))
     RETURNING snapshot_id,(EXTRACT(EPOCH FROM created_at)*1000)::BIGINT",
  )
  .bind(workspace)
  .bind(oid)
  .bind(uid)
  .bind(bytes)
  .bind(days)
  .fetch_one(&mut *tx)
  .await
  .map_err(AppError::from)
  .map_err(permission_error)?;
  tx.commit()
    .await
    .map_err(AppError::from)
    .map_err(permission_error)?;
  Ok(
    AppResponse::Ok()
      .with_data(DocumentVersionMeta {
        snapshot_id: id.to_string(),
        object_id: oid.to_string(),
        created_at,
        reused: false,
      })
      .into(),
  )
}

#[cfg(test)]
mod tests {
  use super::*;
  use collab_document::document_data::default_document_data;

  fn example() -> DocumentData {
    serde_json::from_value(serde_json::json!({
      "page_id": "root", "blocks": {
        "root": {"id":"root", "ty":"page", "parent":"", "children":"root-children", "data":{}},
        "paragraph": {"id":"paragraph", "ty":"paragraph", "parent":"root", "children":"paragraph-children", "external_id":"text", "external_type":"text", "data":{}}
      }, "meta": {"children_map":{"root-children":["paragraph"], "paragraph-children":[]},
        "text_map":{"text":"[{\"insert\":\"hello\"}]"}}
    })).unwrap()
  }

  #[test]
  fn identical_content_ignores_internal_ids_and_unused_maps() {
    let first = example();
    let renamed = serde_json::to_string(&first)
      .unwrap()
      .replace("root", "new-root")
      .replace("paragraph", "new-block")
      .replace("text", "new-text");
    // The block type and external type are content, not IDs.
    let renamed = renamed
      .replace("\"ty\":\"new-block\"", "\"ty\":\"paragraph\"")
      .replace(
        "\"external_type\":\"new-text\"",
        "\"external_type\":\"text\"",
      )
      .replace("\"new-text_map\"", "\"text_map\"");
    let mut second: DocumentData = serde_json::from_str(&renamed).unwrap();
    second
      .meta
      .text_map
      .as_mut()
      .unwrap()
      .insert("unused".into(), "[]".into());
    assert_eq!(
      version_content(&first).unwrap(),
      version_content(&second).unwrap()
    );
  }

  #[test]
  fn identical_text_runs_ignore_crdt_segmentation() {
    let first = example();
    let mut second = example();
    second.meta.text_map.as_mut().unwrap().insert(
      "text".into(),
      r#"[{"insert":"he","attributes":{}},{"insert":"llo"},{"insert":""}]"#.into(),
    );
    assert_eq!(
      version_content(&first).unwrap(),
      version_content(&second).unwrap()
    );
  }

  #[test]
  fn inline_and_external_text_have_the_same_content() {
    let external = example();
    let mut inline = example();
    let block = inline.blocks.get_mut("paragraph").unwrap();
    block.external_id = None;
    block.external_type = None;
    block
      .data
      .insert("delta".into(), serde_json::json!([{"insert":"hello"}]));
    assert_eq!(
      version_content(&external).unwrap(),
      version_content(&inline).unwrap()
    );
  }

  #[test]
  fn text_format_type_and_resource_changes_are_distinct() {
    let first = example();
    for delta in [
      r#"[{"insert":"changed"}]"#,
      r#"[{"insert":"hello","attributes":{"bold":true}}]"#,
    ] {
      let mut changed = example();
      changed
        .meta
        .text_map
        .as_mut()
        .unwrap()
        .insert("text".into(), delta.into());
      assert_ne!(
        version_content(&first).unwrap(),
        version_content(&changed).unwrap()
      );
    }
    let mut changed = example();
    changed.blocks.get_mut("paragraph").unwrap().ty = "heading".into();
    assert_ne!(
      version_content(&first).unwrap(),
      version_content(&changed).unwrap()
    );
    let mut first = example();
    first
      .blocks
      .get_mut("paragraph")
      .unwrap()
      .data
      .insert("url".into(), serde_json::json!("image-a.png"));
    let mut changed = first.clone();
    changed
      .blocks
      .get_mut("paragraph")
      .unwrap()
      .data
      .insert("url".into(), serde_json::json!("image-b.png"));
    assert_ne!(
      version_content(&first).unwrap(),
      version_content(&changed).unwrap()
    );
  }

  #[test]
  fn rejects_missing_root_and_cycles_before_storage() {
    let id = Uuid::new_v4().to_string();
    let mut data = default_document_data(&id);
    assert!(validate_document(&data).is_ok());
    let root_id = data.page_id.clone();
    let children = data.blocks[&root_id].children.clone();
    data.blocks.get_mut(&root_id).unwrap().parent = root_id.clone();
    data
      .meta
      .children_map
      .insert(children, vec![root_id.clone()]);
    assert!(validate_document(&data).is_err());
    data.blocks.remove(&root_id);
    assert!(validate_document(&data).is_err());
  }
}
