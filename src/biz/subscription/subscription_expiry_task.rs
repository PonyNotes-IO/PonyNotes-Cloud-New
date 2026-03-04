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
    match expire_overdue_subscriptions(&pg_pool).await {
      Ok(count) => {
        if count > 0 {
          info!("[订阅过期检查] 已将 {} 条过期订阅状态更新为 expired", count);
        }
      }
      Err(e) => {
        error!("[订阅过期检查] 执行失败: {:?}", e);
      }
    }
  }
}
