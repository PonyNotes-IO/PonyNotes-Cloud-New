-- 2026-03-04-为订阅表增加降级宽限期字段
-- 用于记录用户降级套餐后的资源保留截止时间

ALTER TABLE af_user_subscriptions
ADD COLUMN IF NOT EXISTS grace_period_end TIMESTAMP WITH TIME ZONE DEFAULT NULL;

-- 添加索引以便定时任务查询
CREATE INDEX IF NOT EXISTS idx_af_user_subscriptions_grace_period_end
ON af_user_subscriptions(grace_period_end)
WHERE grace_period_end IS NOT NULL;

-- 添加 downgraded_from_plan_id 字段，记录从哪个套餐降级
ALTER TABLE af_user_subscriptions
ADD COLUMN IF NOT EXISTS downgraded_from_plan_id INTEGER DEFAULT NULL REFERENCES af_subscription_plans(id);
