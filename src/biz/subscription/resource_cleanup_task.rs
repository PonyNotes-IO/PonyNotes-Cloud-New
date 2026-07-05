use std::sync::Arc;
use sqlx::{PgPool, Row};
use app_error::AppError;
use tracing::{info, warn, error, instrument};
use tokio::time::{interval, Duration};
use uuid::Uuid;
use database::file::s3_client_impl::AwsS3BucketClientImpl;
use database::file::BucketClient;
use crate::biz::subscription::ops::get_user_resource_limit_status;

const CLEANUP_INTERVAL_SECS: u64 = 86400;

pub async fn start_resource_cleanup_task(
    pg_pool: PgPool,
    s3_client: AwsS3BucketClientImpl,
    qiniu_client: Option<AwsS3BucketClientImpl>,
) {
    info!("[资源清理] 定时任务已启动，检查间隔: {}秒", CLEANUP_INTERVAL_SECS);

    let s3 = Arc::new(StorageClients {
        minio: s3_client,
        qiniu: qiniu_client,
    });
    let mut timer = interval(Duration::from_secs(CLEANUP_INTERVAL_SECS));

    loop {
        timer.tick().await;
        if let Err(e) = run_resource_cleanup_task(&pg_pool, &s3).await {
            error!("[资源清理] 执行失败: {:?}", e);
        }
    }
}

/// 物理文件可能分布在七牛云（当前主存储）和 MinIO（历史数据）两处，删除时两边都要清。
pub struct StorageClients {
    pub minio: AwsS3BucketClientImpl,
    pub qiniu: Option<AwsS3BucketClientImpl>,
}

impl StorageClients {
    async fn remove_dir_all(&self, dir: &str) {
        if let Some(qiniu) = &self.qiniu {
            if let Err(e) = qiniu.remove_dir(dir).await {
                warn!("[资源清理] 删除七牛云目录 {} 失败: {:?}", dir, e);
            }
        }
        if let Err(e) = self.minio.remove_dir(dir).await {
            warn!("[资源清理] 删除MinIO目录 {} 失败: {:?}", dir, e);
        }
    }

    async fn delete_blobs_all(&self, keys: Vec<String>) {
        if let Some(qiniu) = &self.qiniu {
            if let Err(e) = qiniu.delete_blobs(keys.clone()).await {
                warn!("[资源清理] 批量删除七牛云文件失败: {:?}", e);
            }
        }
        if let Err(e) = self.minio.delete_blobs(keys).await {
            warn!("[资源清理] 批量删除MinIO文件失败: {:?}", e);
        }
    }
}

#[instrument(skip_all)]
pub async fn run_resource_cleanup_task(
    pg_pool: &PgPool,
    s3: &Arc<StorageClients>,
) -> Result<(), AppError> {
    info!("[资源清理] 开始执行资源清理任务...");

    let users_needing_cleanup = database::subscription::get_users_needing_cleanup(pg_pool).await?;

    if users_needing_cleanup.is_empty() {
        info!("[资源清理] 没有需要清理的用户");
        return Ok(());
    }

    info!("[资源清理] 发现 {} 个用户需要检查资源清理", users_needing_cleanup.len());

    for uid in users_needing_cleanup {
        if let Err(e) = check_and_cleanup_user(pg_pool, s3, uid).await {
            warn!("[资源清理] 处理用户 {} 失败: {:?}", uid, e);
        }
    }

    info!("[资源清理] 资源清理任务执行完毕");
    Ok(())
}

async fn check_and_cleanup_user(
    pg_pool: &PgPool,
    s3: &Arc<StorageClients>,
    uid: i64,
) -> Result<(), AppError> {
    let resource_status = get_user_resource_limit_status(pg_pool, uid).await?;

    if resource_status.is_grace_period {
        info!(
            "[资源清理] 用户 {} 仍在宽限期内（至 {:?}），跳过清理",
            uid, resource_status.grace_period_end
        );
        return Ok(());
    }

    let storage_limit_bytes = (resource_status.storage_limit_mb * 1024.0 * 1024.0) as i64;
    let workspace_limit = resource_status.workspace_limit;

    info!(
        "[资源清理] 用户 {} 当前套餐: {}, 存储限额: {} MB, 工作区限额: {}",
        uid, resource_status.plan_code, resource_status.storage_limit_mb, workspace_limit
    );

    cleanup_user_workspaces(pg_pool, s3, uid, workspace_limit).await?;
    cleanup_user_storage(pg_pool, s3, uid, storage_limit_bytes).await?;

    Ok(())
}

async fn cleanup_user_workspaces(
    pg_pool: &PgPool,
    s3: &Arc<StorageClients>,
    uid: i64,
    workspace_limit: i64,
) -> Result<(), AppError> {
    let rows = sqlx::query(
        r#"
        SELECT workspace_id
        FROM af_workspace
        WHERE owner_uid = $1
        ORDER BY created_at ASC
        OFFSET $2
        "#,
    )
    .bind(uid)
    .bind(workspace_limit)
    .fetch_all(pg_pool)
    .await?;

    if rows.is_empty() {
        return Ok(());
    }

    info!(
        "[资源清理] 用户 {} 需要删除 {} 个超出限额的工作区（限额: {}）",
        uid, rows.len(), workspace_limit
    );

    for row in rows {
        let workspace_id: Uuid = row.get("workspace_id");
        info!("[资源清理] 删除工作区 {} (用户 {})", workspace_id, uid);

        // 先删除对象存储（七牛云 + MinIO）中该工作区下的所有文件
        s3.remove_dir_all(&workspace_id.to_string()).await;

        let mut tx = pg_pool.begin().await?;

        sqlx::query("DELETE FROM af_workspace_ai_usage WHERE workspace_id = $1")
            .bind(workspace_id)
            .execute(&mut *tx).await?;
        sqlx::query("DELETE FROM af_collab WHERE workspace_id = $1")
            .bind(workspace_id)
            .execute(&mut *tx).await?;
        sqlx::query("DELETE FROM af_blob_metadata WHERE workspace_id = $1")
            .bind(workspace_id)
            .execute(&mut *tx).await?;
        sqlx::query("DELETE FROM af_workspace_member WHERE workspace_id = $1")
            .bind(workspace_id)
            .execute(&mut *tx).await?;
        sqlx::query("DELETE FROM af_workspace_invitation WHERE workspace_id = $1")
            .bind(workspace_id)
            .execute(&mut *tx).await?;
        sqlx::query("DELETE FROM af_workspace WHERE workspace_id = $1")
            .bind(workspace_id)
            .execute(&mut *tx).await?;

        tx.commit().await?;
    }

    Ok(())
}

async fn cleanup_user_storage(
    pg_pool: &PgPool,
    s3: &Arc<StorageClients>,
    uid: i64,
    storage_limit_bytes: i64,
) -> Result<(), AppError> {
    let current_usage = database::subscription::get_user_total_usage_bytes(pg_pool, uid).await?;

    if current_usage <= storage_limit_bytes {
        return Ok(());
    }

    info!(
        "[资源清理] 用户 {} 存储超限: 当前 {} 字节, 限额 {} 字节, 需释放 {} 字节",
        uid, current_usage, storage_limit_bytes, current_usage - storage_limit_bytes
    );

    let rows = sqlx::query(
        r#"
        SELECT workspace_id, file_id, file_size
        FROM af_blob_metadata
        WHERE workspace_id IN (
            SELECT workspace_id FROM af_workspace WHERE owner_uid = $1
        )
        ORDER BY modified_at ASC
        "#,
    )
    .bind(uid)
    .fetch_all(pg_pool)
    .await?;

    // 按 modified_at ASC 从最旧的文件开始删，直到释放足够空间使总用量降到限额以内。
    // （注意 current_usage 还包含笔记 CRDT 数据与头像，blob 全删完仍可能超限，此时如实记录。）
    let mut to_release: i64 = current_usage - storage_limit_bytes;
    let mut blobs_to_delete: Vec<(Uuid, String)> = Vec::new();

    for row in rows {
        if to_release <= 0 {
            break;
        }
        let file_size: i64 = row.get("file_size");
        let workspace_id: Uuid = row.get("workspace_id");
        let file_id: String = row.get("file_id");
        blobs_to_delete.push((workspace_id, file_id));
        to_release -= file_size;
    }

    if to_release > 0 {
        warn!(
            "[资源清理] 用户 {} 删除全部 {} 个文件后仍超限 {} 字节（超限部分为笔记数据/头像，暂不清理）",
            uid, blobs_to_delete.len(), to_release
        );
    }

    if !blobs_to_delete.is_empty() {
        info!(
            "[资源清理] 用户 {} 需删除 {} 个超出限额的文件",
            uid, blobs_to_delete.len()
        );

        // 先批量删除对象存储文件（七牛云 + MinIO）
        let s3_keys: Vec<String> = blobs_to_delete
            .iter()
            .map(|(ws_id, file_id)| format!("{}/{}", ws_id, file_id))
            .collect();

        s3.delete_blobs_all(s3_keys).await;

        // 再删除数据库元数据
        for (workspace_id, file_id) in blobs_to_delete {
            sqlx::query(
                "DELETE FROM af_blob_metadata WHERE workspace_id = $1 AND file_id = $2",
            )
            .bind(workspace_id)
            .bind(&file_id)
            .execute(pg_pool)
            .await?;
        }
    }

    Ok(())
}
