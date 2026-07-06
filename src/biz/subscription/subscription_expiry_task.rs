use sqlx::PgPool;
use tracing::{info, error};
use tokio::time::{interval, Duration};
use database::subscription::expire_overdue_subscriptions;

const EXPIRY_CHECK_INTERVAL_SECS: u64 = 3600; // 每小时检查一次

pub async fn start_subscription_expiry_task(pg_pool: PgPool) {
  info!(
    "[订阅过期检查] 定时任务已启动，检查间隔: {}秒",
    EXPIRY_CHECK_INTERVAL_SECS
  );

  let mut timer = interval(Duration::from_secs(EXPIRY_CHECK_INTERVAL_SECS));

  loop {
    timer.tick().await;
    // 按规则：付费套餐到期不续费 → 直接降级为免费版。
    // 这里只负责把到期订阅标记为 expired；免费版由用户下次请求时
    // get_or_create_free_subscription 自动补发，超限内容由资源清理任务
    // 在 15 天宽限期后做逻辑删除。
    match expire_overdue_subscriptions(&pg_pool).await {
      Ok(uids) => {
        if !uids.is_empty() {
          info!("[订阅过期检查] 已将 {} 条过期订阅状态更新为 expired", uids.len());
        }
      }
      Err(e) => {
        error!("[订阅过期检查] 执行失败: {:?}", e);
      }
    }
  }
}
