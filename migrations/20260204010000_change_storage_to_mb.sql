-- 2026-02-04-更新云存储字段单位并清理无用数据
-- 1. 将 cloud_storage_gb 字段改为 DECIMAL(10,2)，单位从 GB 改为 MB
-- 2. 清理无用的重复数据

-- 开始事务
BEGIN;

-- Step 1: 添加新的临时字段存储 MB 值
ALTER TABLE af_subscription_plans ADD COLUMN cloud_storage_mb DECIMAL(10, 2);

-- Step 2: 将 GB 转换为 MB（乘以 1024）
UPDATE af_subscription_plans SET cloud_storage_mb = cloud_storage_gb * 1024;

-- Step 3: 删除旧的 GB 字段
ALTER TABLE af_subscription_plans DROP COLUMN cloud_storage_gb;

-- Step 4: 将新字段重命名为 cloud_storage_gb（现在实际存储的是 MB）
ALTER TABLE af_subscription_plans RENAME COLUMN cloud_storage_mb TO cloud_storage_gb;

-- Step 5: 更新免费版的云存储为 300 MB
UPDATE af_subscription_plans SET cloud_storage_gb = 300 WHERE plan_code = 'mfb';

-- Step 6: 清理无用的数据 - 删除 stand 重复记录
DELETE FROM af_subscription_plans WHERE plan_code = 'stand';

-- 验证更新结果
SELECT 
    id, plan_code, plan_name_cn, 
    cloud_storage_gb,
    CASE 
        WHEN cloud_storage_gb < 1024 THEN cloud_storage_gb || ' MB'
        ELSE ROUND(cloud_storage_gb / 1024, 2) || ' GB'
    END as storage_display,
    ai_chat_count_per_month,
    collaborative_workspace_limit,
    monthly_price_yuan, yearly_price_yuan,
    is_active
FROM af_subscription_plans
ORDER BY id;

COMMIT;
