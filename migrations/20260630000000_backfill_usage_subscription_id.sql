-- 修复 af_user_subscription_usage.subscription_id
-- 背景：
--   1) 记录 AI 使用量时从未写入 subscription_id（历史数据全为 NULL），
--      导致无法按"当前订阅套餐"维度统计剩余次数；
--   2) subscription_id 仍为 INTEGER，与 af_user_subscriptions.id (BIGINT) 类型不一致，
--      Rust 侧使用 i64 绑定会失配。

-- 1. 类型对齐为 BIGINT（与 af_user_subscriptions.id 一致，匹配 Rust i64）
ALTER TABLE af_user_subscription_usage ALTER COLUMN subscription_id TYPE BIGINT;

-- 2. 回填历史数据：把每条使用记录归属到该用户在 usage_date 当天有效的订阅
--    匹配规则：usage_date 落在订阅 [start_date, end_date] 区间内；
--    若有多条命中，优先 active，其次取 id 最大（最近创建）的一条。
UPDATE af_user_subscription_usage u
SET subscription_id = (
  SELECT s.id
  FROM af_user_subscriptions s
  WHERE s.uid = u.uid
    AND u.usage_date >= s.start_date::date
    AND u.usage_date <= s.end_date::date
  ORDER BY (s.status = 'active') DESC, s.id DESC
  LIMIT 1
)
WHERE u.subscription_id IS NULL;

-- 3. 重建唯一索引：把订阅维度纳入唯一键。
--    这样套餐切换/续费（生成新的 subscription_id）后，新套餐用量从 0 重新累计，
--    实现"只统计当前套餐使用次数"的语义。
DROP INDEX IF EXISTS idx_af_user_subscription_usage_unique;
CREATE UNIQUE INDEX IF NOT EXISTS idx_af_user_subscription_usage_unique
  ON af_user_subscription_usage(uid, subscription_id, usage_date, usage_type);

-- 4. 加一个按订阅查询用量的复合索引，加速剩余次数统计
CREATE INDEX IF NOT EXISTS idx_af_user_subscription_usage_sub
  ON af_user_subscription_usage(uid, subscription_id, usage_type);
