use app_error::AppError;
use chrono::{DateTime, Utc};
use database_entity::dto::{
  PatchPublishedCollab, PublishCollabItem, PublishCollabKey, PublishInfo, WorkspaceNamespace,
};
use sqlx::{Executor, PgPool, Postgres, QueryBuilder};
use uuid::Uuid;

use crate::pg_row::AFPublishViewWithPublishInfo;

pub async fn select_user_is_collab_publisher_for_all_views(
  pg_pool: &PgPool,
  user_uuid: &Uuid,
  workspace_uuid: &Uuid,
  view_ids: &[Uuid],
) -> Result<bool, AppError> {
  let count = sqlx::query_scalar!(
    r#"
      SELECT COUNT(*)
      FROM af_published_collab
      WHERE workspace_id = $1
        AND view_id = ANY($2)
        AND published_by = (SELECT uid FROM af_user WHERE uuid = $3)
    "#,
    workspace_uuid,
    view_ids,
    user_uuid,
  )
  .fetch_one(pg_pool)
  .await?;

  match count {
    Some(c) => Ok(c == view_ids.len() as i64),
    None => Ok(false),
  }
}

#[inline]
pub async fn select_workspace_publish_namespace_exists<'a, E: Executor<'a, Database = Postgres>>(
  executor: E,
  namespace: &str,
) -> Result<bool, AppError> {
  let res = sqlx::query_scalar!(
    r#"
      SELECT EXISTS(
        SELECT 1
        FROM af_workspace_namespace
        WHERE namespace = $1
      )
    "#,
    namespace,
  )
  .fetch_one(executor)
  .await?;

  Ok(res.unwrap_or(false))
}

#[inline]
pub async fn insert_non_orginal_workspace_publish_namespace(
  pg_pool: &PgPool,
  workspace_id: &Uuid,
  new_namespace: &str,
) -> Result<(), AppError> {
  let res = sqlx::query!(
    r#"
      INSERT INTO af_workspace_namespace
      VALUES ($1, $2, FALSE)
    "#,
    new_namespace,
    workspace_id,
  )
  .execute(pg_pool)
  .await?;

  if res.rows_affected() != 1 {
    tracing::error!(
      "Failed to insert workspace publish namespace, workspace_id: {}, new_namespace: {}, rows_affected: {}",
      workspace_id, new_namespace, res.rows_affected()
    );
  }

  Ok(())
}

#[inline]
pub async fn update_non_orginal_workspace_publish_namespace(
  pg_pool: &PgPool,
  workspace_id: &Uuid,
  old_namespace: &str,
  new_namespace: &str,
) -> Result<(), AppError> {
  let res = sqlx::query!(
    r#"
      UPDATE af_workspace_namespace
      SET namespace = $1
      WHERE workspace_id = $2
        AND namespace = $3
        AND is_original = FALSE
    "#,
    new_namespace,
    workspace_id,
    old_namespace,
  )
  .execute(pg_pool)
  .await?;

  if res.rows_affected() != 1 {
    tracing::error!(
      "Failed to update workspace publish namespace, workspace_id: {}, new_namespace: {}, rows_affected: {}",
      workspace_id, new_namespace, res.rows_affected()
    );
  }

  Ok(())
}

#[inline]
pub async fn update_workspace_default_publish_view<'a, E: Executor<'a, Database = Postgres>>(
  executor: E,
  workspace_id: &Uuid,
  new_view_id: &Uuid,
) -> Result<(), AppError> {
  let res = sqlx::query!(
    r#"
      UPDATE af_workspace
      SET default_published_view_id = $1
      WHERE workspace_id = $2
    "#,
    new_view_id,
    workspace_id,
  )
  .execute(executor)
  .await?;

  if res.rows_affected() != 1 {
    tracing::error!(
        "Failed to update workspace default publish view, workspace_id: {}, new_view_id: {}, rows_affected: {}",
        workspace_id, new_view_id, res.rows_affected()
    );
  }

  Ok(())
}

#[inline]
pub async fn update_workspace_default_publish_view_set_null<
  'a,
  E: Executor<'a, Database = Postgres>,
>(
  executor: E,
  workspace_id: &Uuid,
) -> Result<(), AppError> {
  let res = sqlx::query!(
    r#"
      UPDATE af_workspace
      SET default_published_view_id = NULL
      WHERE workspace_id = $1
    "#,
    workspace_id,
  )
  .execute(executor)
  .await?;

  if res.rows_affected() != 1 {
    tracing::error!(
      "Failed to unset workspace default publish view, workspace_id: {}, rows_affected: {}",
      workspace_id,
      res.rows_affected()
    );
  }

  Ok(())
}

#[inline]
pub async fn select_workspace_publish_namespaces(
  pg_pool: &PgPool,
  workspace_id: &Uuid,
) -> Result<Vec<WorkspaceNamespace>, AppError> {
  let res = sqlx::query_as!(
    WorkspaceNamespace,
    r#"
      SELECT workspace_id, namespace, is_original
      FROM af_workspace_namespace
      WHERE workspace_id = $1
    "#,
    workspace_id,
  )
  .fetch_all(pg_pool)
  .await?;

  Ok(res)
}

#[inline]
pub async fn select_workspace_publish_namespace(
  pg_pool: &PgPool,
  workspace_id: &Uuid,
  namespace: &str,
) -> Result<WorkspaceNamespace, AppError> {
  let res = sqlx::query_as!(
    WorkspaceNamespace,
    r#"
      SELECT workspace_id, namespace, is_original
      FROM af_workspace_namespace
      WHERE workspace_id = $1
        AND namespace = $2
    "#,
    workspace_id,
    namespace,
  )
  .fetch_one(pg_pool)
  .await?;

  Ok(res)
}

async fn delete_published_collabs(
  txn: &mut sqlx::Transaction<'_, Postgres>,
  workspace_id: &Uuid,
  publish_names: &[String],
) -> Result<(), AppError> {
  let delete_publish_names = sqlx::query_scalar!(
    r#"
      DELETE FROM af_published_collab
      WHERE workspace_id = $1
        AND publish_name = ANY($2::text[])
      RETURNING publish_name
    "#,
    workspace_id,
    &publish_names,
  )
  .fetch_all(txn.as_mut())
  .await?;
  if !delete_publish_names.is_empty() {
    tracing::info!(
      "Deleted existing published collab record with publish names: {:?}",
      delete_publish_names
    );
  }
  Ok(())
}

#[inline]
pub async fn insert_or_replace_publish_collabs(
  pg_pool: &PgPool,
  workspace_id: &Uuid,
  publisher_uuid: &Uuid,
  publish_items: Vec<PublishCollabItem<serde_json::Value, Vec<u8>>>,
) -> Result<(), AppError> {
  let item_count = publish_items.len();
  let mut view_ids: Vec<Uuid> = Vec::with_capacity(item_count);
  let mut publish_names: Vec<String> = Vec::with_capacity(item_count);
  let mut metadatas: Vec<serde_json::Value> = Vec::with_capacity(item_count);
  let mut blobs: Vec<Vec<u8>> = Vec::with_capacity(item_count);
  let mut comments_enabled_list: Vec<bool> = Vec::with_capacity(item_count);
  let mut duplicate_enabled_list: Vec<bool> = Vec::with_capacity(item_count);
  publish_items.into_iter().for_each(|item| {
    view_ids.push(item.meta.view_id);
    publish_names.push(item.meta.publish_name);
    metadatas.push(item.meta.metadata);
    blobs.push(item.data);
    comments_enabled_list.push(item.comments_enabled);
    duplicate_enabled_list.push(item.duplicate_enabled);
  });

  let mut txn = pg_pool.begin().await?;
  delete_published_collabs(&mut txn, workspace_id, &publish_names).await?;

  let res = sqlx::query!(
    r#"
      INSERT INTO af_published_collab (workspace_id, view_id, publish_name, published_by, metadata, blob, comments_enabled, duplicate_enabled)
      SELECT * FROM UNNEST(
        (SELECT array_agg((SELECT $1::uuid)) FROM generate_series(1, $9))::uuid[],
        $2::uuid[],
        $3::text[],
        (SELECT array_agg((SELECT uid FROM af_user WHERE uuid = $4)) FROM generate_series(1, $9))::bigint[],
        $5::jsonb[],
        $6::bytea[],
        $7::boolean[],
        $8::boolean[]
      )
      ON CONFLICT (workspace_id, view_id) DO UPDATE
      SET metadata = EXCLUDED.metadata,
          blob = EXCLUDED.blob,
          published_by = EXCLUDED.published_by,
          publish_name = EXCLUDED.publish_name
    "#,
    workspace_id,
    &view_ids,
    &publish_names,
    publisher_uuid,
    &metadatas,
    &blobs,
    &comments_enabled_list,
    &duplicate_enabled_list,
    item_count as i32,
  )
  .execute(txn.as_mut())
  .await?;

  if res.rows_affected() != item_count as u64 {
    tracing::warn!(
      "Failed to insert or replace publish collab meta batch, workspace_id: {}, publisher_uuid: {}, rows_affected: {}, item_count: {}",
      workspace_id, publisher_uuid, res.rows_affected(), item_count
    );
  }

  txn.commit().await?;
  Ok(())
}

#[inline]
pub async fn select_publish_collab_meta<'a, E: Executor<'a, Database = Postgres>>(
  executor: E,
  publish_namespace: &str,
  publish_name: &str,
) -> Result<serde_json::Value, AppError> {
  let res = sqlx::query!(
    r#"
    SELECT metadata
    FROM af_published_collab
    WHERE workspace_id = (SELECT workspace_id FROM af_workspace_namespace WHERE namespace = $1)
      AND unpublished_at IS NULL
      AND publish_name = $2
    "#,
    publish_namespace,
    publish_name,
  )
  .fetch_one(executor)
  .await?;
  let metadata: serde_json::Value = res.metadata;
  Ok(metadata)
}

#[inline]
pub async fn set_published_collabs_as_unpublished<'a, E: Executor<'a, Database = Postgres>>(
  executor: E,
  workspace_id: &Uuid,
  view_ids: &[Uuid],
) -> Result<(), AppError> {
  let res = sqlx::query!(
    r#"
      UPDATE af_published_collab
      SET
        blob = E''::bytea,
        unpublished_at = NOW()
      WHERE workspace_id = $1
        AND view_id = ANY($2)
    "#,
    workspace_id,
    view_ids,
  )
  .execute(executor)
  .await?;

  if res.rows_affected() != view_ids.len() as u64 {
    tracing::error!(
      "Failed to delete published collabs, workspace_id: {}, view_ids: {:?}, rows_affected: {}",
      workspace_id,
      view_ids,
      res.rows_affected()
    );
  }

  Ok(())
}

#[inline]
pub async fn update_published_collabs(
  txn: &mut sqlx::Transaction<'_, Postgres>,
  workspace_id: &Uuid,
  patches: &[PatchPublishedCollab],
) -> Result<(), AppError> {
  {
    // Delete existing published collab records with the same publish names
    let publish_names: Vec<String> = patches
      .iter()
      .filter_map(|patch| patch.publish_name.clone())
      .collect();
    delete_published_collabs(txn, workspace_id, &publish_names).await?;
  }
  for patch in patches {
    let mut query_builder: QueryBuilder<Postgres> = QueryBuilder::new(
      r#"
        UPDATE af_published_collab SET
      "#,
    );
    let mut first_set = true;
    if let Some(comments_enabled) = patch.comments_enabled {
      first_set = false;
      query_builder.push(" comments_enabled = ");
      query_builder.push_bind(comments_enabled);
    }
    if let Some(duplicate_enabled) = patch.duplicate_enabled {
      if !first_set {
        query_builder.push(",");
      }
      first_set = false;
      query_builder.push(" duplicate_enabled = ");
      query_builder.push_bind(duplicate_enabled);
    }
    if let Some(publish_name) = &patch.publish_name {
      if !first_set {
        query_builder.push(",");
      }
      query_builder.push(" publish_name = ");
      query_builder.push_bind(publish_name);
    }
    query_builder.push(" WHERE workspace_id = ");
    query_builder.push_bind(workspace_id);
    query_builder.push(" AND view_id = ");
    query_builder.push_bind(patch.view_id);
    let query = query_builder.build();
    let res = query.execute(txn.as_mut()).await?;

    if res.rows_affected() != 1 {
      tracing::error!(
          "Failed to update published collab publish name, workspace_id: {}, view_id: {}, rows_affected: {}",
          workspace_id,
          patch.view_id,
          res.rows_affected()
        );
    }
  }

  Ok(())
}

#[inline]
pub async fn select_published_metadata_for_view_id(
  pg_pool: &PgPool,
  view_id: &Uuid,
) -> Result<Option<(Uuid, serde_json::Value)>, AppError> {
  let res = sqlx::query!(
    r#"
      SELECT workspace_id, metadata
      FROM af_published_collab
      WHERE view_id = $1
    "#,
    view_id,
  )
  .fetch_optional(pg_pool)
  .await?;
  Ok(res.map(|res| (res.workspace_id, res.metadata)))
}

#[inline]
pub async fn select_published_data_for_view_id(
  pg_pool: &PgPool,
  view_id: &Uuid,
) -> Result<Option<(serde_json::Value, Vec<u8>)>, AppError> {
  let res = sqlx::query!(
    r#"
      SELECT metadata, blob
      FROM af_published_collab
      WHERE view_id = $1
    "#,
    view_id,
  )
  .fetch_optional(pg_pool)
  .await?;
  Ok(res.map(|res| (res.metadata, res.blob)))
}

#[inline]
pub async fn select_published_collab_workspace_view_id<'a, E: Executor<'a, Database = Postgres>>(
  executor: E,
  publish_namespace: &str,
  publish_name: &str,
) -> Result<PublishCollabKey, AppError> {
  let key = sqlx::query_as!(
    PublishCollabKey,
    r#"
      SELECT workspace_id, view_id
      FROM af_published_collab
      WHERE workspace_id = (SELECT workspace_id FROM af_workspace_namespace WHERE namespace = $1)
      AND publish_name = $2
    "#,
    publish_namespace,
    publish_name,
  )
  .fetch_one(executor)
  .await?;
  Ok(key)
}

#[inline]
pub async fn select_published_collab_blob<'a, E: Executor<'a, Database = Postgres>>(
  executor: E,
  publish_namespace: &str,
  publish_name: &str,
) -> Result<Vec<u8>, AppError> {
  let res = sqlx::query_scalar!(
    r#"
      SELECT blob
      FROM af_published_collab
      WHERE workspace_id = (SELECT workspace_id FROM af_workspace_namespace WHERE namespace = $1)
        AND unpublished_at IS NULL
        AND publish_name = $2
    "#,
    publish_namespace,
    publish_name,
  )
  .fetch_one(executor)
  .await?;

  Ok(res)
}

pub async fn select_default_published_view_id_for_namespace<
  'a,
  E: Executor<'a, Database = Postgres>,
>(
  executor: E,
  namespace: &str,
) -> Result<Option<Uuid>, AppError> {
  let res = sqlx::query_scalar!(
    r#"
      SELECT default_published_view_id
      FROM af_workspace
      WHERE workspace_id = (SELECT workspace_id FROM af_workspace_namespace WHERE namespace = $1)
    "#,
    namespace,
  )
  .fetch_one(executor)
  .await?;

  Ok(res)
}

pub async fn select_default_published_view_id<'a, E: Executor<'a, Database = Postgres>>(
  executor: E,
  workspace_id: &Uuid,
) -> Result<Option<Uuid>, AppError> {
  let res = sqlx::query_scalar!(
    r#"
      SELECT default_published_view_id
      FROM af_workspace
      WHERE workspace_id = $1
    "#,
    workspace_id,
  )
  .fetch_one(executor)
  .await?;

  Ok(res)
}

async fn select_most_recent_non_original_namespace(
  pg_pool: &PgPool,
  namespace: &str,
) -> Result<Option<String>, AppError> {
  let res = sqlx::query_scalar!(
    r#"
      SELECT namespace
      FROM af_workspace_namespace
      WHERE workspace_id = (SELECT workspace_id FROM af_workspace_namespace WHERE namespace = $1)
        AND is_original = FALSE
      ORDER BY created_at DESC
      LIMIT 1
    "#,
    namespace,
  )
  .fetch_optional(pg_pool)
  .await?;

  Ok(res)
}

pub async fn select_publish_info_for_view_ids(
  pg_pool: &PgPool,
  view_ids: &[Uuid],
) -> Result<Vec<PublishInfo>, AppError> {
  let mut res = sqlx::query_as!(
    PublishInfo,
    r#"
      SELECT
        awn.namespace,
        apc.publish_name,
        apc.view_id,
        au.email AS publisher_email,
        apc.created_at AS publish_timestamp,
        apc.unpublished_at AS unpublished_timestamp,
        apc.comments_enabled,
        apc.duplicate_enabled
      FROM af_published_collab apc
      JOIN af_user au ON apc.published_by = au.uid
      JOIN af_workspace aw ON apc.workspace_id = aw.workspace_id
      JOIN af_workspace_namespace awn ON aw.workspace_id = awn.workspace_id AND awn.is_original = TRUE
      WHERE apc.view_id = ANY($1);
    "#,
    view_ids,
  )
  .fetch_all(pg_pool)
  .await?;

  if res.is_empty() {
    return Ok(res);
  }
  if let Some(non_original_namespace) =
    select_most_recent_non_original_namespace(pg_pool, &res[0].namespace).await?
  {
    res.iter_mut().for_each(|info| {
      info.namespace.clone_from(&non_original_namespace);
    });
  }
  Ok(res)
}

pub async fn select_published_collab_info(
  pg_pool: &PgPool,
  view_id: &Uuid,
) -> Result<PublishInfo, AppError> {
  select_publish_info_for_view_ids(pg_pool, &[*view_id])
    .await?
    .into_iter()
    .next()
    .ok_or(AppError::RecordNotFound(view_id.to_string()))
}

pub async fn select_all_published_collab_info(
  pg_pool: &PgPool,
  workspace_id: &Uuid,
) -> Result<Vec<PublishInfo>, AppError> {
  let mut res = sqlx::query_as!(
    PublishInfo,
    r#"
      SELECT
        awn.namespace,
        apc.publish_name,
        apc.view_id,
        au.email AS publisher_email,
        apc.created_at AS publish_timestamp,
        apc.unpublished_at AS unpublished_timestamp,
        apc.comments_enabled,
        apc.duplicate_enabled
      FROM af_published_collab apc
      JOIN af_user au ON apc.published_by = au.uid
      JOIN af_workspace aw ON apc.workspace_id = aw.workspace_id
      JOIN af_workspace_namespace awn ON aw.workspace_id = awn.workspace_id AND awn.is_original = TRUE
      WHERE apc.workspace_id = $1 AND apc.unpublished_at IS NULL;
    "#,
    workspace_id,
  )
  .fetch_all(pg_pool)
  .await?;

  use_non_orginal_namespace_if_possible(pg_pool, &mut res).await?;
  Ok(res)
}

/// 查询所有发布的笔记（不限制 workspace_id）
/// 用于侧边栏发布菜单显示所有发布的笔记
pub async fn select_all_published_collab_info_global(
  pg_pool: &PgPool,
) -> Result<Vec<PublishInfo>, AppError> {
  let mut res = sqlx::query_as!(
    PublishInfo,
    r#"
      SELECT
        awn.namespace,
        apc.publish_name,
        apc.view_id,
        au.email AS publisher_email,
        apc.created_at AS publish_timestamp,
        apc.unpublished_at AS unpublished_timestamp,
        apc.comments_enabled,
        apc.duplicate_enabled
      FROM af_published_collab apc
      JOIN af_user au ON apc.published_by = au.uid
      JOIN af_workspace aw ON apc.workspace_id = aw.workspace_id
      JOIN af_workspace_namespace awn ON aw.workspace_id = awn.workspace_id AND awn.is_original = TRUE
      WHERE apc.unpublished_at IS NULL
      ORDER BY apc.created_at DESC;
    "#,
  )
  .fetch_all(pg_pool)
  .await?;

  // 使用非原始命名空间（如果有的话）
  if res.is_empty() {
    return Ok(res);
  }

  // 获取所有唯一的命名空间并更新
  let mut seen_namespaces = std::collections::HashSet::<String>::new();
  for info in res.iter_mut() {
    if !seen_namespaces.contains(&info.namespace) {
      if let Some(non_original_namespace) =
        select_most_recent_non_original_namespace(pg_pool, &info.namespace).await?
      {
        info.namespace.clone_from(&non_original_namespace);
      }
      seen_namespaces.insert(info.namespace.clone());
    }
  }

  Ok(res)
}

/// 查询当前用户自己发布的文档
pub async fn select_published_collab_by_uid(
  pg_pool: &PgPool,
  uid: i64,
) -> Result<Vec<PublishInfo>, AppError> {
  let mut res = sqlx::query_as!(
    PublishInfo,
    r#"
      SELECT
        awn.namespace,
        apc.publish_name,
        apc.view_id,
        au.email AS publisher_email,
        apc.created_at AS publish_timestamp,
        apc.unpublished_at AS unpublished_timestamp,
        apc.comments_enabled,
        apc.duplicate_enabled
      FROM af_published_collab apc
      JOIN af_user au ON apc.published_by = au.uid
      JOIN af_workspace aw ON apc.workspace_id = aw.workspace_id
      JOIN af_workspace_namespace awn ON aw.workspace_id = awn.workspace_id AND awn.is_original = TRUE
      WHERE apc.published_by = $1 AND apc.unpublished_at IS NULL
      ORDER BY apc.created_at DESC;
    "#,
    uid,
  )
  .fetch_all(pg_pool)
  .await?;

  use_non_orginal_namespace_if_possible(pg_pool, &mut res).await?;
  Ok(res)
}

/// 查询用户接收的发布文档及其发布详情（含名称、发布者邮箱等）
pub async fn select_received_published_collab_with_details(
  pg_pool: &PgPool,
  uid: i64,
) -> Result<Vec<ReceivedPublishedCollabDetail>, AppError> {
  let res = sqlx::query_as!(
    ReceivedPublishedCollabDetail,
    r#"
      SELECT
        rpc.published_view_id,
        rpc.view_id AS received_view_id,
        rpc.workspace_id,
        rpc.published_at,
        rpc.is_readonly,
        COALESCE(apc.publish_name, '') AS "publish_name!",
        COALESCE(awn.namespace, '') AS "namespace!",
        au.email AS publisher_email
      FROM af_received_published_collab rpc
      LEFT JOIN af_published_collab apc ON rpc.published_view_id = apc.view_id
      LEFT JOIN af_workspace aw ON apc.workspace_id = aw.workspace_id
      LEFT JOIN af_workspace_namespace awn ON aw.workspace_id = awn.workspace_id AND awn.is_original = TRUE
      LEFT JOIN af_user au ON rpc.published_by = au.uid
      WHERE rpc.received_by = $1
      ORDER BY rpc.received_at DESC
    "#,
    uid,
  )
  .fetch_all(pg_pool)
  .await?;

  Ok(res)
}

/// 接收的发布文档详情（含发布信息）
#[derive(sqlx::FromRow, Debug)]
pub struct ReceivedPublishedCollabDetail {
  pub published_view_id: Uuid,
  pub received_view_id: Uuid,
  pub workspace_id: Uuid,
  pub published_at: DateTime<Utc>,
  pub is_readonly: bool,
  pub publish_name: String,
  pub namespace: String,
  pub publisher_email: Option<String>,
}

async fn use_non_orginal_namespace_if_possible(
  pg_pool: &PgPool,
  publish_infos: &mut [PublishInfo],
) -> Result<(), AppError> {
  if publish_infos.is_empty() {
    return Ok(());
  }

  if let Some(non_original_namespace) =
    select_most_recent_non_original_namespace(pg_pool, &publish_infos[0].namespace).await?
  {
    publish_infos.iter_mut().for_each(|info| {
      info.namespace.clone_from(&non_original_namespace);
    });
  }
  Ok(())
}

pub async fn select_workspace_id_for_publish_namespace<'a, E: Executor<'a, Database = Postgres>>(
  executor: E,
  publish_namespace: &str,
) -> Result<Uuid, AppError> {
  let res = sqlx::query!(
    r#"
      SELECT workspace_id
      FROM af_workspace_namespace
      WHERE namespace = $1
    "#,
    publish_namespace,
  )
  .fetch_one(executor)
  .await?;

  Ok(res.workspace_id)
}

pub async fn select_published_view_ids_for_workspace<'a, E: Executor<'a, Database = Postgres>>(
  executor: E,
  workspace_id: Uuid,
) -> Result<Vec<Uuid>, AppError> {
  let res = sqlx::query_scalar!(
    r#"
      SELECT view_id
      FROM af_published_collab
      WHERE workspace_id = $1
      AND unpublished_at IS NULL
    "#,
    workspace_id,
  )
  .fetch_all(executor)
  .await?;

  Ok(res)
}

pub async fn select_published_view_ids_with_publish_info_for_workspace<
  'a,
  E: Executor<'a, Database = Postgres>,
>(
  executor: E,
  workspace_id: Uuid,
) -> Result<Vec<AFPublishViewWithPublishInfo>, AppError> {
  let res = sqlx::query_as!(
    AFPublishViewWithPublishInfo,
    r#"
      SELECT
        apc.view_id,
        apc.publish_name,
        au.email AS publisher_email,
        apc.created_at AS publish_timestamp,
        apc.comments_enabled,
        apc.duplicate_enabled
      FROM af_published_collab apc
      JOIN af_user au ON apc.published_by = au.uid
      WHERE workspace_id = $1
      AND unpublished_at IS NULL
    "#,
    workspace_id,
  )
  .fetch_all(executor)
  .await?;

  Ok(res)
}

use crate::pg_row::AFReceivedPublishedCollab;

/// 查询用户接收到的所有发布文档
pub async fn select_received_published_collabs(
  pg_pool: &PgPool,
  uid: i64,
) -> Result<Vec<AFReceivedPublishedCollab>, AppError> {
  let res = sqlx::query_as!(
    AFReceivedPublishedCollab,
    r#"
      SELECT
        received_by,
        published_view_id,
        workspace_id,
        view_id,
        published_by,
        published_at,
        received_at,
        is_readonly
      FROM af_received_published_collab
      WHERE received_by = $1
      ORDER BY received_at DESC
    "#,
    uid,
  )
  .fetch_all(pg_pool)
  .await?;

  Ok(res)
}

/// 查询用户接收的特定发布文档
pub async fn select_received_published_collab(
  pg_pool: &PgPool,
  uid: i64,
  published_view_id: &Uuid,
) -> Result<Option<AFReceivedPublishedCollab>, AppError> {
  let res = sqlx::query_as!(
    AFReceivedPublishedCollab,
    r#"
      SELECT
        received_by,
        published_view_id,
        workspace_id,
        view_id,
        published_by,
        published_at,
        received_at,
        is_readonly
      FROM af_received_published_collab
      WHERE received_by = $1 AND published_view_id = $2
    "#,
    uid,
    published_view_id,
  )
  .fetch_optional(pg_pool)
  .await?;

  Ok(res)
}

/// 插入用户接收的发布文档记录
pub async fn insert_received_published_collab(
  pg_pool: &PgPool,
  received_by: i64,
  published_view_id: &Uuid,
  workspace_id: &Uuid,
  view_id: &Uuid,
  published_by: i64,
  published_at: &DateTime<Utc>,
  is_readonly: bool,
) -> Result<(), AppError> {
  let res = sqlx::query!(
    r#"
      INSERT INTO af_received_published_collab
        (received_by, published_view_id, workspace_id, view_id, published_by, published_at, is_readonly)
      VALUES ($1, $2, $3, $4, $5, $6, $7)
      ON CONFLICT (received_by, published_view_id) DO NOTHING
    "#,
    received_by,
    published_view_id,
    workspace_id,
    view_id,
    published_by,
    published_at,
    is_readonly,
  )
  .execute(pg_pool)
  .await?;

  if res.rows_affected() != 1 {
    tracing::warn!(
      "Failed to insert received published collab, received_by: {}, published_view_id: {}, rows_affected: {}",
      received_by, published_view_id, res.rows_affected()
    );
  }

  Ok(())
}

/// 删除用户接收的发布文档记录
pub async fn delete_received_published_collab(
  pg_pool: &PgPool,
  uid: i64,
  published_view_id: &Uuid,
) -> Result<(), AppError> {
  let _res = sqlx::query!(
    r#"
      DELETE FROM af_received_published_collab
      WHERE received_by = $1 AND published_view_id = $2
    "#,
    uid,
    published_view_id,
  )
  .execute(pg_pool)
  .await?;

  Ok(())
}

/// 查询发布文档的详情（包含发布者和接收者信息）
pub async fn select_published_collab_with_receivers(
  pg_pool: &PgPool,
  view_id: &Uuid,
) -> Result<Option<(AFPublishViewWithPublishInfo, Vec<AFReceivedPublishedCollab>)>, AppError> {
  // 获取发布文档信息
  let publish_info = sqlx::query_as!(
    AFPublishViewWithPublishInfo,
    r#"
      SELECT
        apc.view_id,
        apc.publish_name,
        au.email AS publisher_email,
        apc.created_at AS publish_timestamp,
        apc.comments_enabled,
        apc.duplicate_enabled
      FROM af_published_collab apc
      JOIN af_user au ON apc.published_by = au.uid
      WHERE apc.view_id = $1 AND apc.unpublished_at IS NULL
    "#,
    view_id,
  )
  .fetch_optional(pg_pool)
  .await?;

  match publish_info {
    Some(info) => {
      // 获取所有接收者
      let receivers = sqlx::query_as!(
        AFReceivedPublishedCollab,
        r#"
          SELECT
            received_by,
            published_view_id,
            workspace_id,
            view_id,
            published_by,
            published_at,
            received_at,
            is_readonly
          FROM af_received_published_collab
          WHERE published_view_id = $1
        "#,
        view_id,
      )
      .fetch_all(pg_pool)
      .await?;

      Ok(Some((info, receivers)))
    },
    None => Ok(None),
  }
}
