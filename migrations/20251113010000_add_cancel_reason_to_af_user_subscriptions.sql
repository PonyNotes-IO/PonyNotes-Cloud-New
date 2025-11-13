-- 为用户订阅表添加取消原因字段
-- 该字段用于记录用户取消订阅时的说明/原因

ALTER TABLE af_user_subscriptions
  ADD COLUMN IF NOT EXISTS cancel_reason TEXT;


