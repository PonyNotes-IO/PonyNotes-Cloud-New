use sqlx::{PgPool, Row};
use app_error::AppError;
use tracing::{info, warn, error, instrument};
use tokio::time::{interval, Duration};
use uuid::Uuid;
use crate::biz::subscription::ops::get_user_resource_limit_status;

const CLEANUP_INTERVAL_SECS: u64 = 86400;

/// 过期会员资源清理定时任务。
///
/// 按规则全部使用**逻辑删除**：只在数据库打 deleted_at 标记，
/// 对象存储文件与数据库内容全部保留，客户端表现为已删除且禁止访问；
/// 用户重新购买足够限额的套餐后可自动恢复（见 ops.rs 的恢复逻辑）。
pub async fn start_resource_cleanup_task(pg_pool: PgPool) {
    info!("[资源清理] 定时任务已启动（逻辑删除模式），检查间隔: {}秒", CLEANUP_INTERVAL_SECS);

    let mut timer = interval(Duration::from_secs(CLEANUP_INTERVAL_SECS));

    loop {
        timer.tick().await;
        if let Err(e) = run_resource_cleanup_task(&pg_pool).await {
            error!("[资源清理] 执行失败: {:?}", e);
        }
    }
}

#[instrument(skip_all)]
pub async fn run_resource_cleanup_task(pg_pool: &PgPool) -> Result<(), AppError> {
    info!("[资源清理] 开始执行资源清理任务...");

    let users_needing_cleanup = database::subscription::get_users_needing_cleanup(pg_pool).await?;

    if users_needing_cleanup.is_empty() {
        info!("[资源清理] 没有需要清理的用户");
        return Ok(());
    }

    info!("[资源清理] 发现 {} 个用户需要检查资源清理", users_needing_cleanup.len());

    for uid in users_needing_cleanup {
        if let Err(e) = check_and_cleanup_user(pg_pool, uid).await {
            warn!("[资源清理] 处理用户 {} 失败: {:?}", uid, e);
        }
    }

    info!("[资源清理] 资源清理任务执行完毕");
    Ok(())
}

async fn check_and_cleanup_user(pg_pool: &PgPool, uid: i64) -> Result<(), AppError> {
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

    // 逻辑删除超出限额的工作区（保留最早创建的 workspace_limit 个）
    let removed_workspaces =
        database::subscription::soft_delete_workspaces_over_limit(pg_pool, uid, workspace_limit)
            .await?;
    if removed_workspaces > 0 {
        info!(
            "[资源清理] 用户 {} 逻辑删除 {} 个超出限额的工作区（限额: {}）",
            uid, removed_workspaces, workspace_limit
        );
    }

    cleanup_user_storage(pg_pool, uid, storage_limit_bytes).await?;

    Ok(())
}

async fn cleanup_user_storage(
    pg_pool: &PgPool,
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

    // 只统计未删除工作区中未删除的 blob
    let rows = sqlx::query(
        r#"
        SELECT b.workspace_id, b.file_id, b.file_size
        FROM af_blob_metadata b
        JOIN af_workspace w ON w.workspace_id = b.workspace_id
        WHERE w.owner_uid = $1 AND w.deleted_at IS NULL AND b.deleted_at IS NULL
        ORDER BY b.modified_at ASC
        "#,
    )
    .bind(uid)
    .fetch_all(pg_pool)
    .await?;

    // 按 modified_at ASC 从最旧的文件开始逻辑删除，直到总用量降到限额以内。
    // （current_usage 还包含笔记 CRDT 数据与头像，blob 全删完仍可能超限，此时如实记录。）
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
            "[资源清理] 用户 {} 逻辑删除全部 {} 个文件后仍超限 {} 字节（超限部分为笔记数据/头像，暂不清理）",
            uid, blobs_to_delete.len(), to_release
        );
    }

    if !blobs_to_delete.is_empty() {
        info!(
            "[资源清理] 用户 {} 需逻辑删除 {} 个超出限额的文件（对象存储保留，可恢复）",
            uid, blobs_to_delete.len()
        );

        for (workspace_id, file_id) in blobs_to_delete {
            sqlx::query(
                "UPDATE af_blob_metadata SET deleted_at = NOW() WHERE workspace_id = $1 AND file_id = $2",
            )
            .bind(workspace_id)
            .bind(&file_id)
            .execute(pg_pool)
            .await?;
        }
    }

    Ok(())
}
