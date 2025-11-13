-- 修复订阅相关表的ID字段类型不匹配问题
-- 问题：Rust代码期望i64 (BIGINT)，但数据库表使用了INTEGER类型
-- 解决方案：将所有相关表的ID字段改为BIGINT类型

-- 1. 修复af_subscription_plans表的ID字段
ALTER TABLE af_subscription_plans ALTER COLUMN id TYPE BIGINT;

-- 2. 修复af_subscription_addons表的ID字段
ALTER TABLE af_subscription_addons ALTER COLUMN id TYPE BIGINT;

-- 3. 修复af_user_subscriptions表的ID和外键字段
ALTER TABLE af_user_subscriptions ALTER COLUMN id TYPE BIGINT;
ALTER TABLE af_user_subscriptions ALTER COLUMN plan_id TYPE BIGINT;

-- 4. 修复af_user_addons表的ID和外键字段
ALTER TABLE af_user_addons ALTER COLUMN id TYPE BIGINT;
ALTER TABLE af_user_addons ALTER COLUMN addon_id TYPE BIGINT;

-- 5. 修复af_user_subscription_usage表的ID字段
ALTER TABLE af_user_subscription_usage ALTER COLUMN id TYPE BIGINT;