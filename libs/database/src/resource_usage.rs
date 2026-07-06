use crate::pg_row::AFBlobMetadataRow;
use app_error::AppError;
use rust_decimal::prelude::ToPrimitive;
use sqlx::types::Decimal;
use sqlx::{Executor, PgPool, Postgres, Transaction};
use std::ops::DerefMut;

use tracing::instrument;
use uuid::Uuid;

#[instrument(level = "trace", skip_all)]
#[inline]
pub async fn is_blob_metadata_exists(
  pool: &PgPool,
  workspace_id: &Uuid,
  file_id: &str,
) -> Result<bool, AppError> {
  // 逻辑删除(deleted_at 非空)的记录视为不存在, 允许重新上传时走 upsert 复活
  let exists: (bool,) = sqlx::query_as(
    r#"
     SELECT EXISTS (
         SELECT 1
         FROM af_blob_metadata
         WHERE workspace_id = $1 AND file_id = $2 AND deleted_at IS NULL
     );
    "#,
  )
  .bind(workspace_id)
  .bind(file_id)
  .fetch_one(pool)
  .await?;

  Ok(exists.0)
}

#[instrument(level = "trace", skip_all, err)]
pub async fn insert_blob_metadata(
  pg_pool: &PgPool,
  file_id: &str,
  workspace_id: &Uuid,
  file_type: &str,
  file_size: usize,
) -> Result<(), AppError> {
  // 重新上传同名文件时清除逻辑删除标记(复活), 使用运行时查询以避免重新生成 .sqlx 缓存
  let res = sqlx::query(
    r#"
        INSERT INTO af_blob_metadata
        (workspace_id, file_id, file_type, file_size)
        VALUES ($1, $2, $3, $4)
        ON CONFLICT (workspace_id, file_id) DO UPDATE SET
            file_type = $3,
            file_size = $4,
            deleted_at = NULL
        "#,
  )
  .bind(workspace_id)
  .bind(file_id)
  .bind(file_type)
  .bind(file_size as i64)
  .execute(pg_pool)
  .await?;
  let n = res.rows_affected();
  if n != 1 {
    tracing::error!("insert_blob_metadata: rows_affected: {}", n);
  }
  Ok(())
}

#[derive(Debug, Clone)]
pub struct BulkInsertMeta {
  pub object_id: String,
  pub file_id: String,
  pub file_type: String,
  pub file_size: i64,
}

#[instrument(level = "trace", skip_all, err)]
pub async fn insert_blob_metadata_bulk<'a, E: Executor<'a, Database = Postgres>>(
  executor: E,
  workspace_id: &Uuid,
  metadata: Vec<BulkInsertMeta>,
) -> Result<u64, sqlx::Error> {
  let mut file_ids = Vec::with_capacity(metadata.len());
  let mut file_types = Vec::with_capacity(metadata.len());
  let mut file_sizes = Vec::with_capacity(metadata.len());

  for BulkInsertMeta {
    object_id,
    file_id,
    file_type,
    file_size,
  } in metadata
  {
    // we use BlobPathV1 to generate file_id
    file_ids.push(format!("{}_{}", object_id, file_id));
    file_types.push(file_type);
    file_sizes.push(file_size);
  }
  let query = r#"
        INSERT INTO af_blob_metadata (workspace_id, file_id, file_type, file_size)
        SELECT $1, unnest($2::text[]), unnest($3::text[]), unnest($4::int8[])
        ON CONFLICT DO NOTHING
    "#;

  let result = sqlx::query(query)
    .bind(workspace_id)
    .bind(file_ids)
    .bind(file_types)
    .bind(file_sizes)
    .execute(executor)
    .await?;

  Ok(result.rows_affected())
}
#[instrument(level = "trace", skip_all, err)]
#[inline]
pub async fn delete_blob_metadata(
  tx: &mut Transaction<'_, sqlx::Postgres>,
  workspace_id: &Uuid,
  file_id: &str,
) -> Result<(), AppError> {
  let result = sqlx::query!(
    r#"
        DELETE FROM af_blob_metadata
        WHERE workspace_id = $1 AND file_id = $2
        "#,
    workspace_id,
    file_id,
  )
  .execute(tx.deref_mut())
  .await?;
  let n = result.rows_affected();
  tracing::info!("delete_blob_metadata: rows_affected: {}", n);
  Ok(())
}

#[instrument(level = "trace", skip_all, err)]
pub async fn get_blob_metadata(
  pg_pool: &PgPool,
  workspace_id: &Uuid,
  metadata_key: &str,
) -> Result<AFBlobMetadataRow, AppError> {
  tracing::trace!(
    "get_blob_metadata: workspace_id: {}, metadata_key: {}",
    workspace_id,
    metadata_key
  );
  // file_id is the BlobPath's blob_metadata_key
  // 逻辑删除(deleted_at 非空)的文件对外表现为不存在(下载返回 404), 对象存储保留可恢复
  let metadata = sqlx::query_as::<_, AFBlobMetadataRow>(
    r#"
        SELECT workspace_id, file_id, file_type, file_size, modified_at, status, source, source_metadata
        FROM af_blob_metadata
        WHERE workspace_id = $1 AND file_id = $2 AND deleted_at IS NULL
        "#,
  )
  .bind(workspace_id)
  .bind(metadata_key)
  .fetch_one(pg_pool)
  .await?;
  Ok(metadata)
}

/// Return all blob metadata of a workspace
#[instrument(level = "trace", skip_all, err)]
#[inline]
pub async fn get_all_workspace_blob_metadata(
  pg_pool: &PgPool,
  workspace_id: &Uuid,
) -> Result<Vec<AFBlobMetadataRow>, AppError> {
  // 排除逻辑删除的文件, 使用运行时查询以避免重新生成 .sqlx 缓存
  let all_metadata = sqlx::query_as::<_, AFBlobMetadataRow>(
    r#"
        SELECT workspace_id, file_id, file_type, file_size, modified_at, status, source, source_metadata
        FROM af_blob_metadata
        WHERE workspace_id = $1 AND deleted_at IS NULL
        "#,
  )
  .bind(workspace_id)
  .fetch_all(pg_pool)
  .await?;
  Ok(all_metadata)
}

/// Return all blob ids of a workspace
#[instrument(level = "trace", skip_all, err)]
#[inline]
pub async fn get_all_workspace_blob_ids(
  pg_pool: &PgPool,
  workspace_id: &Uuid,
) -> Result<Vec<String>, AppError> {
  let file_ids = sqlx::query!(
    r#"
    SELECT file_id FROM af_blob_metadata
    WHERE workspace_id = $1
    "#,
    workspace_id
  )
  .fetch_all(pg_pool)
  .await?
  .into_iter()
  .map(|record| record.file_id)
  .collect();
  Ok(file_ids)
}

/// Return the total size of a workspace in bytes
#[instrument(level = "trace", skip_all, err)]
#[inline]
pub async fn get_workspace_usage_size(pool: &PgPool, workspace_id: &Uuid) -> Result<u64, AppError> {
  let row: (Option<Decimal>,) =
    sqlx::query_as(
      r#"SELECT SUM(file_size) FROM af_blob_metadata WHERE workspace_id = $1 AND deleted_at IS NULL;"#,
    )
      .bind(workspace_id)
      .fetch_one(pool)
      .await?;
  match row.0 {
    Some(decimal) => Ok(decimal.to_u64().unwrap_or(0)),
    None => Ok(0),
  }
}
