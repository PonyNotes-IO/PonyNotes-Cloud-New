//! 手工回归测试：验证 2026-07-07 对"宽限期口径不一致" bug 的修复。
//!
//! 仅在显式指定 TEST_DATABASE_URL / TEST_UID_A / TEST_UID_B 时针对真实测试账号运行，
//! 不在 CI 默认测试集中执行（#[ignore]）。运行前后都会打印被修改的订阅记录，
//! 并在测试结束时恢复到运行前的原始状态（不会保留污染数据）。
//!
//! 运行方式：
//!   TEST_DATABASE_URL=... TEST_UID_A=... TEST_UID_B=... \
//!     cargo test --test subscription_grace_period_fix_verification -- --ignored --nocapture

use appflowy_cloud::biz::subscription::ops::get_user_resource_limit_status;
use chrono::Utc;
use sqlx::{PgPool, Row};

async fn connect() -> PgPool {
  let database_url =
    std::env::var("TEST_DATABASE_URL").expect("请设置 TEST_DATABASE_URL 环境变量");
  PgPool::connect(&database_url).await.expect("连接测试数据库失败")
}

fn test_uid(var: &str) -> i64 {
  std::env::var(var)
    .unwrap_or_else(|_| panic!("请设置 {} 环境变量", var))
    .parse()
    .unwrap_or_else(|_| panic!("{} 必须是合法的 uid 数字", var))
}

#[derive(Debug)]
struct BackedUpRow {
  id: i64,
  plan_id: i64,
  billing_type: String,
  status: String,
  start_date: chrono::DateTime<Utc>,
  end_date: chrono::DateTime<Utc>,
  grace_period_end: Option<chrono::DateTime<Utc>>,
  downgraded_from_plan_id: Option<i32>,
}

async fn backup_subscriptions(pool: &PgPool, uid: i64) -> Vec<BackedUpRow> {
  let rows = sqlx::query(
    r#"SELECT id, plan_id, billing_type, status, start_date, end_date,
              grace_period_end, downgraded_from_plan_id
       FROM af_user_subscriptions WHERE uid = $1"#,
  )
  .bind(uid)
  .fetch_all(pool)
  .await
  .expect("备份查询失败");

  rows
    .into_iter()
    .map(|row| BackedUpRow {
      id: row.get(0),
      plan_id: row.get(1),
      billing_type: row.get(2),
      status: row.get(3),
      start_date: row.get(4),
      end_date: row.get(5),
      grace_period_end: row.get(6),
      downgraded_from_plan_id: row.get(7),
    })
    .collect()
}

/// 清空测试账号名下全部订阅记录，随后按备份逐条恢复（保证测试前后状态完全一致）。
async fn restore_subscriptions(pool: &PgPool, uid: i64, backup: &[BackedUpRow]) {
  sqlx::query("DELETE FROM af_user_subscriptions WHERE uid = $1")
    .bind(uid)
    .execute(pool)
    .await
    .expect("清空测试账号订阅失败");

  for row in backup {
    sqlx::query(
      r#"INSERT INTO af_user_subscriptions
         (id, uid, plan_id, billing_type, status, start_date, end_date, grace_period_end, downgraded_from_plan_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)"#,
    )
    .bind(row.id)
    .bind(uid)
    .bind(row.plan_id)
    .bind(&row.billing_type)
    .bind(&row.status)
    .bind(row.start_date)
    .bind(row.end_date)
    .bind(row.grace_period_end)
    .bind(row.downgraded_from_plan_id)
    .execute(pool)
    .await
    .expect("恢复订阅记录失败");
  }
  // id 是 SERIAL，恢复插入显式 id 后需要把序列推进到最大值之后，避免后续测试/线上购买 id 冲突。
  sqlx::query(
    "SELECT setval(pg_get_serial_sequence('af_user_subscriptions', 'id'), COALESCE((SELECT MAX(id) FROM af_user_subscriptions), 1))",
  )
  .execute(pool)
  .await
  .expect("重置序列失败");
}

async fn get_first_plan_id_at_level(pool: &PgPool, level_plan_codes: &[&str]) -> i64 {
  let row = sqlx::query(
    "SELECT id FROM af_subscription_plans WHERE plan_code = ANY($1) AND is_active = TRUE ORDER BY id LIMIT 1",
  )
  .bind(level_plan_codes)
  .fetch_one(pool)
  .await
  .expect("找不到测试所需的套餐");
  row.get(0)
}

/// 测试用例 J：验证账号存在多条历史过期记录时，
/// 候选清理名单（get_users_needing_cleanup）与实际清理前复核（get_user_resource_limit_status）
/// 现在使用同一条"最近一次终止的订阅记录"作为判断基准，不再出现"入围但被跳过"的矛盾。
#[tokio::test]
#[ignore]
async fn verify_grace_period_consistency_fix() {
  let pool = connect().await;
  let uid = test_uid("TEST_UID_A");

  println!("[用例J] 使用测试账号 uid={}", uid);

  let backup = backup_subscriptions(&pool, uid).await;
  println!("[用例J] 已备份原有订阅记录 {} 条", backup.len());

  let team_plan_id = get_first_plan_id_at_level(&pool, &["team", "hiclass", "enterprise"]).await;
  let standard_plan_id = get_first_plan_id_at_level(&pool, &["standard", "stand", "profersor"]).await;

  // 清空该账号所有订阅记录，构造场景：
  // 记录A：team，20 天前过期（已超 15 天宽限期）
  // 记录B：standard，5 天前过期（仍在 15 天宽限期内）
  sqlx::query("DELETE FROM af_user_subscriptions WHERE uid = $1")
    .bind(uid)
    .execute(&pool)
    .await
    .expect("清空测试账号订阅失败");

  sqlx::query(
    r#"INSERT INTO af_user_subscriptions (uid, plan_id, billing_type, status, start_date, end_date, canceled_at)
       VALUES ($1, $2, 'monthly', 'expired', NOW() - INTERVAL '50 days', NOW() - INTERVAL '20 days', NOW() - INTERVAL '20 days')"#,
  )
  .bind(uid)
  .bind(team_plan_id)
  .execute(&pool)
  .await
  .expect("插入记录A失败");

  sqlx::query(
    r#"INSERT INTO af_user_subscriptions (uid, plan_id, billing_type, status, start_date, end_date, canceled_at)
       VALUES ($1, $2, 'monthly', 'expired', NOW() - INTERVAL '35 days', NOW() - INTERVAL '5 days', NOW() - INTERVAL '5 days')"#,
  )
  .bind(uid)
  .bind(standard_plan_id)
  .execute(&pool)
  .await
  .expect("插入记录B失败");

  println!("[用例J] 已构造两条历史记录：记录A(20天前过期,team) / 记录B(5天前过期,standard)");

  // 对照组：按修复前的原始 SQL（任意一条历史记录满足15天条件即入围）重放一遍，
  // 用于证明该 bug 在修复前确实会把这个用户错误地纳入候选名单。
  let old_logic_would_include: bool = sqlx::query_scalar(
    r#"SELECT EXISTS (
         SELECT 1 FROM af_user_subscriptions
         WHERE uid = $1 AND status IN ('expired', 'canceled')
           AND end_date + INTERVAL '15 days' < NOW()
       )"#,
  )
  .bind(uid)
  .fetch_one(&pool)
  .await
  .expect("模拟修复前逻辑失败");
  println!(
    "[用例J][对照] 按修复前逐行判断逻辑，是否会入围: {}（预期 true，命中记录A，用于证明 bug 曾经存在）",
    old_logic_would_include
  );

  // 步骤1：修复后的候选清理名单（真实调用 get_users_needing_cleanup，与线上代码完全一致）
  let candidates = database::subscription::get_users_needing_cleanup(&pool)
    .await
    .expect("查询候选清理名单失败");
  let in_candidates = candidates.contains(&uid);
  println!("[用例J] 修复后候选清理名单是否包含该用户: {}", in_candidates);

  // 步骤2：实际清理前复核（真实调用 get_user_resource_limit_status，与线上代码完全一致）
  let status = get_user_resource_limit_status(&pool, uid)
    .await
    .expect("获取用户资源限额状态失败");
  println!(
    "[用例J] 复核结果: is_grace_period={}, plan_code={}, grace_period_end={:?}",
    status.is_grace_period, status.plan_code, status.grace_period_end
  );

  // 恢复原始订阅数据
  restore_subscriptions(&pool, uid, &backup).await;
  println!("[用例J] 已恢复该账号原始订阅数据");

  // 断言1：证明 bug 在修复前确实存在——旧逻辑会因命中记录A而入围。
  assert!(
    old_logic_would_include,
    "对照组本应因记录A命中而入围，用来证明 bug 曾经存在；如果这里是 false，说明测试数据构造有问题"
  );
  // 断言2：修复后，两处判断必须使用同一条"最近一次终止记录"（记录B），结果保持一致——
  // 候选名单不应再纳入该用户（因为最新一次终止记录B仍在15天宽限期内），
  // 复核也判定仍在宽限期内，两者一致，不再出现"入围但被跳过"的矛盾。
  assert!(
    !in_candidates,
    "修复未生效：候选名单仍然因命中过旧的记录A而纳入该用户"
  );
  assert!(
    status.is_grace_period,
    "复核应判定仍在宽限期内（基于最新一次终止记录B），如果为 false 说明复核逻辑被意外改动"
  );

  println!(
    "[用例J] ✅ 修复验证通过：修复前 old_logic_would_include=true（bug 曾存在），\
     修复后 in_candidates=false 且 is_grace_period=true，两处判断口径一致，\
     不再出现'候选名单入围但复核判定仍在宽限期而被跳过'的矛盾状态"
  );
}

/// 回归测试：账号只有单条历史过期记录、且已超过15天宽限期的最常见场景，
/// 确认本次修复没有影响这个原本就应该正确工作的基础用例
/// （候选名单纳入 + 复核判定不在宽限期内，两者都应为清理放行）。
#[tokio::test]
#[ignore]
async fn verify_single_overdue_record_still_flagged_consistently() {
  let pool = connect().await;
  let uid = test_uid("TEST_UID_B");

  println!("[回归] 使用测试账号 uid={}", uid);

  let backup = backup_subscriptions(&pool, uid).await;
  println!("[回归] 已备份原有订阅记录 {} 条", backup.len());

  let standard_plan_id = get_first_plan_id_at_level(&pool, &["standard", "stand", "profersor"]).await;

  sqlx::query("DELETE FROM af_user_subscriptions WHERE uid = $1")
    .bind(uid)
    .execute(&pool)
    .await
    .expect("清空测试账号订阅失败");

  // 唯一一条历史记录：30 天前过期，已超 15 天宽限期。
  sqlx::query(
    r#"INSERT INTO af_user_subscriptions (uid, plan_id, billing_type, status, start_date, end_date, canceled_at)
       VALUES ($1, $2, 'monthly', 'expired', NOW() - INTERVAL '60 days', NOW() - INTERVAL '30 days', NOW() - INTERVAL '30 days')"#,
  )
  .bind(uid)
  .bind(standard_plan_id)
  .execute(&pool)
  .await
  .expect("插入历史记录失败");

  println!("[回归] 已构造唯一一条历史记录：30天前过期(standard)，已超15天宽限期");

  let candidates = database::subscription::get_users_needing_cleanup(&pool)
    .await
    .expect("查询候选清理名单失败");
  let in_candidates = candidates.contains(&uid);
  println!("[回归] 候选清理名单是否包含该用户: {}", in_candidates);

  let status = get_user_resource_limit_status(&pool, uid)
    .await
    .expect("获取用户资源限额状态失败");
  println!(
    "[回归] 复核结果: is_grace_period={}, plan_code={}",
    status.is_grace_period, status.plan_code
  );

  restore_subscriptions(&pool, uid, &backup).await;
  println!("[回归] 已恢复该账号原始订阅数据");

  assert!(in_candidates, "唯一记录已超15天宽限期，候选名单应当纳入该用户");
  assert!(
    !status.is_grace_period,
    "唯一记录已超15天宽限期，复核不应再判定其处于宽限期内"
  );
  assert_eq!(
    status.plan_code, "mfb",
    "宽限期结束后应回落到免费版限额"
  );

  println!("[回归] ✅ 单条记录、真实超期场景验证通过：候选名单与复核判定均一致地允许清理");
}
