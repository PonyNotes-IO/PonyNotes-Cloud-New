use sqlx::PgPool;
use app_error::AppError;
use tracing::{info, warn, instrument};
use crate::biz::subscription::ops::get_user_resource_limit_status;

/// Executes the resource cleanup for all users.
#[instrument(skip_all)]
pub async fn run_resource_cleanup_task(pg_pool: &PgPool) -> Result<(), AppError> {
    info!("Starting resource cleanup task...");
    
    // We iterate through all users who have had any subscription in the past
    // to check if they need cleanup. For a production system, this could be optimized
    // by only checking users modified recently or flagged for cleanup.
    let historical_users = sqlx::query!(
        "SELECT DISTINCT uid FROM af_user_subscriptions"
    )
    .fetch_all(pg_pool)
    .await?;

    for user in historical_users {
        if let Err(e) = check_and_cleanup_user(pg_pool, user.uid).await {
            warn!("Failed to process user {}: {:?}", user.uid, e);
        }
    }

    info!("Resource cleanup task completed.");
    Ok(())
}

async fn check_and_cleanup_user(pg_pool: &PgPool, uid: i64) -> Result<(), AppError> {
    let resource_status = get_user_resource_limit_status(pg_pool, uid).await?;
    
    // If user is in grace period, skip cleanup
    if resource_status.is_grace_period {
        info!("User {} is in grace period until {:?}, skipping cleanup", uid, resource_status.grace_period_end);
        return Ok(());
    }

    let storage_limit_bytes = (resource_status.storage_limit_mb * 1024.0 * 1024.0) as i64;
    let workspace_limit = resource_status.workspace_limit;

    cleanup_user_resources(pg_pool, uid, storage_limit_bytes, workspace_limit).await
}

async fn cleanup_user_resources(
    pg_pool: &PgPool, 
    uid: i64, 
    storage_limit_bytes: i64, 
    workspace_limit: i64
) -> Result<(), AppError> {
    // 1. Truncate collaborative workspaces (Keep earliest N)
    let workspaces_to_delete = sqlx::query!(
        r#"
        SELECT workspace_id
        FROM af_workspace
        WHERE owner_uid = $1
        ORDER BY created_at ASC
        OFFSET $2
        "#,
        uid,
        workspace_limit
    )
    .fetch_all(pg_pool)
    .await?;

    for ws in workspaces_to_delete {
        info!("Deleting workspace {} for user {} (Limit: {})", ws.workspace_id, uid, workspace_limit);
        let mut tx = pg_pool.begin().await?;
        sqlx::query!("DELETE FROM af_workspace_member WHERE workspace_id = $1", ws.workspace_id)
            .execute(&mut *tx).await?;
        sqlx::query!("DELETE FROM af_workspace_invitation WHERE workspace_id = $1", ws.workspace_id)
            .execute(&mut *tx).await?;
        sqlx::query!("DELETE FROM af_workspace WHERE workspace_id = $1", ws.workspace_id)
            .execute(&mut *tx).await?;
        tx.commit().await?;
    }

    // 2. Truncate storage (Keep earliest X bytes)
    let blobs = sqlx::query!(
        r#"
        SELECT workspace_id, file_id, file_size
        FROM af_blob_metadata
        WHERE workspace_id IN (
            SELECT workspace_id FROM af_workspace WHERE owner_uid = $1
        )
        ORDER BY modified_at ASC
        "#,
        uid
    )
    .fetch_all(pg_pool)
    .await?;

    let mut total_size: i64 = 0;
    let mut blobs_to_delete = Vec::new();

    for blob in blobs {
        total_size += blob.file_size;
        if total_size > storage_limit_bytes {
            blobs_to_delete.push((blob.workspace_id, blob.file_id));
        }
    }

    if !blobs_to_delete.is_empty() {
        info!("Deleting {} blobs for user {} (Limit: {} bytes)", blobs_to_delete.len(), uid, storage_limit_bytes);
        for (workspace_id, file_id) in blobs_to_delete {
            sqlx::query!(
                "DELETE FROM af_blob_metadata WHERE workspace_id = $1 AND file_id = $2",
                workspace_id, file_id
            )
            .execute(pg_pool)
            .await?;
        }
    }

    Ok(())
}
