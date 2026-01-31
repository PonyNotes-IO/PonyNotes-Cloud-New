-- 修复 af_subscription_plans 表中的字段类型
-- cloud_storage_gb 字段应该是 INTEGER 类型，但被错误地设置为了 NUMERIC
-- 这导致 Rust 代码解析失败

-- 修复 cloud_storage_gb 字段类型
ALTER TABLE af_subscription_plans 
ALTER COLUMN cloud_storage_gb TYPE INTEGER 
USING cloud_storage_gb::INTEGER;
