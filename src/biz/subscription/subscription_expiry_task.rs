use sqlx::PgPool;
use std::collections::HashSet;
use tracing::{info, error};
use tokio::time::{interval, Duration};
use database::subscription::{
  expire_overdue_subscriptions, get_user_active_subscription, resume_highest_pending_subscription,
};

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
      Ok(uids) => {
        if !uids.is_empty() {
          info!("[订阅过期检查] 已将 {} 条过期订阅状态更新为 expired", uids.len());
        }
        // 对自然到期的用户，逐个尝试恢复升级前暂存的、等级最高的 pending 低级套餐
        //（去重；仅当该用户当前确无有效 active 时才恢复）。
        for uid in uids.into_iter().collect::<HashSet<_>>() {
          match get_user_active_subscription(&pg_pool, uid).await {
            Ok(Some(_)) => {} // 仍有其它有效 active，无需恢复
            Ok(None) => {
              if let Err(e) = resume_highest_pending_subscription(&pg_pool, uid).await {
                error!("[订阅过期检查] uid {} 恢复暂存套餐失败: {:?}", uid, e);
              }
            }
            Err(e) => error!("[订阅过期检查] uid {} 查询当前订阅失败: {:?}", uid, e),
          }
        }
      }
      Err(e) => {
        error!("[订阅过期检查] 执行失败: {:?}", e);
      }
    }
  }
}
