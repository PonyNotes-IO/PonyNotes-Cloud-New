use super::{automatic::active_document_path, permission_error, PAGE_SIZE};
use crate::{
  biz::{authentication::jwt::UserUuid, collab::folder_view::private_space_and_trash_view_ids},
  state::AppState,
};
use access_control::act::Action;
use actix_web::web;
use app_error::AppError;
use appflowy_collaborate::ws2::WorkspaceCollabInstanceCache;
use serde::Deserialize;
use shared_entity::{
  dto::history_dto::{DocumentHistoryIndex, DocumentHistorySummary},
  response::{AppResponse, JsonAppResponse},
};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

#[derive(Deserialize)]
pub struct IndexQuery {
  #[serde(default)]
  search: String,
  #[serde(default)]
  workspace: String,
  #[serde(default)]
  before: i64,
  #[serde(default)]
  before_document: String,
}

#[derive(sqlx::FromRow)]
struct Candidate {
  workspace_id: Uuid,
  document_id: Uuid,
  workspace_name: String,
  latest_version_at: i64,
  is_member: bool,
  is_direct: bool,
}

pub async fn index(
  user: UserUuid,
  state: web::Data<AppState>,
  query: web::Query<IndexQuery>,
) -> actix_web::Result<JsonAppResponse<DocumentHistoryIndex>> {
  let result = list_index(&state, &user, query.into_inner())
    .await
    .map_err(permission_error)?;
  Ok(AppResponse::Ok().with_data(result).into())
}

async fn list_index(
  state: &AppState,
  user: &Uuid,
  query: IndexQuery,
) -> Result<DocumentHistoryIndex, AppError> {
  let uid = state.user_cache.get_user_uid(user).await?;
  let parse_id = |value: &str| {
    (!value.is_empty())
      .then(|| Uuid::parse_str(value))
      .transpose()
  };
  let workspace = parse_id(&query.workspace)?;
  let mut before_document = parse_id(&query.before_document)?.unwrap_or(Uuid::max());
  let mut before = if query.before > 0 {
    query.before
  } else {
    i64::MAX
  };
  let search = query.search.trim().to_lowercase();
  let mut folders = HashMap::new();
  let mut items = Vec::new();
  loop {
    let rows = sqlx::query_as::<_, Candidate>(
      "WITH candidates AS (
         SELECT w.workspace_id, v.oid AS document_id, COALESCE(w.workspace_name, '') AS workspace_name,
           (EXTRACT(EPOCH FROM MAX(v.created_at))*1000)::bigint AS latest_version_at,
           COALESCE(wm.role_id <> 3, FALSE) AS is_member, cm.uid IS NOT NULL AS is_direct
         FROM af_document_version v
         JOIN af_workspace w ON w.workspace_id=v.workspace_id AND w.deleted_at IS NULL
         JOIN af_collab c ON c.workspace_id=v.workspace_id AND c.oid=v.oid
           AND c.partition_key=0 AND c.deleted_at IS NULL
         LEFT JOIN af_workspace_member wm ON wm.workspace_id=w.workspace_id AND wm.uid=$1
         LEFT JOIN af_collab_member cm ON cm.oid=v.oid::text AND cm.uid=$1
         JOIN LATERAL (
           SELECT p.version_history_days FROM af_user_subscriptions s
           JOIN af_subscription_plans p ON p.id=s.plan_id
           WHERE s.uid=w.owner_uid AND s.status='active'
             AND s.start_date<=NOW() AND s.end_date>NOW()
           ORDER BY s.end_date DESC, s.id DESC LIMIT 1
         ) plan ON plan.version_history_days > 0
         WHERE ($2::uuid IS NULL OR w.workspace_id=$2)
           AND (wm.role_id <> 3 OR cm.uid IS NOT NULL)
           AND v.expires_at>NOW() AND v.created_at>=NOW()-make_interval(days=>plan.version_history_days)
         GROUP BY w.workspace_id, v.oid, wm.role_id, cm.uid
       )
       SELECT * FROM candidates
       WHERE (latest_version_at, document_id) < ($3, $4)
       ORDER BY latest_version_at DESC, document_id DESC LIMIT 100",
    )
    .bind(uid).bind(workspace).bind(before).bind(before_document)
    .fetch_all(&state.pg_pool).await?;
    if rows.is_empty() {
      break;
    }
    let is_last_batch = rows.len() < 100;
    for row in rows {
      before = row.latest_version_at;
      before_document = row.document_id;
      match state
        .collab_access_control
        .enforce_action(&row.workspace_id, &uid, &row.document_id, Action::Write)
        .await
      {
        Ok(()) => {},
        Err(AppError::NotEnoughPermissions) => continue,
        Err(err) => return Err(err),
      }
      if let std::collections::hash_map::Entry::Vacant(entry) = folders.entry(row.workspace_id) {
        let folder = state.ws_server.get_folder(row.workspace_id).await?;
        let sections = private_space_and_trash_view_ids(uid, &folder)?;
        let trash = sections
          .view_ids_in_trash
          .iter()
          .map(Uuid::to_string)
          .collect::<HashSet<_>>();
        let private = sections
          .other_private_space_ids
          .iter()
          .map(Uuid::to_string)
          .collect::<HashSet<_>>();
        entry.insert((folder, trash, private));
      }
      let (folder, trash, private) = &folders[&row.workspace_id];
      let Some(path) = active_document_path(folder, row.workspace_id, row.document_id, uid, trash)
      else {
        continue;
      };
      let is_private = path.iter().any(|view| private.contains(&view.id));
      if is_private && !row.is_direct {
        continue;
      }
      let name = path[0].name.clone();
      if !name.to_lowercase().contains(&search) {
        continue;
      }
      let path = if row.is_member && !is_private {
        path
          .iter()
          .skip(1)
          .rev()
          .map(|view| view.name.clone())
          .collect()
      } else {
        Vec::new()
      };
      items.push(DocumentHistorySummary {
        workspace_id: row.workspace_id,
        document_id: row.document_id,
        name,
        workspace_name: if row.is_member {
          row.workspace_name
        } else {
          String::new()
        },
        path,
        latest_version_at: row.latest_version_at,
      });
      if items.len() > PAGE_SIZE as usize {
        items.pop();
        return Ok(DocumentHistoryIndex {
          items,
          has_more: true,
        });
      }
    }
    if is_last_batch {
      break;
    }
  }
  Ok(DocumentHistoryIndex {
    items,
    has_more: false,
  })
}
