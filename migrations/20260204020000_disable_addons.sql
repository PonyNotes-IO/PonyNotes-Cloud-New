-- 2026-02-04-禁用附加包(addon)相关表
-- 由于业务调整，附加包功能已移除，此迁移将禁用 addon 相关数据

-- 开始事务
BEGIN;

-- Step 1: 将所有附加包标记为非活跃
UPDATE af_subscription_addons SET is_active = FALSE;

-- Step 2: 将所有用户附加包标记为已过期
UPDATE af_user_addons SET status = 'expired';

-- Step 3: 删除所有用户附加包记录（可选，如需保留数据可跳过此步）
-- TRUNCATE TABLE af_user_addons CASCADE;

-- 验证结果
SELECT 
    id, addon_code, addon_name_cn, addon_type, 
    price_yuan, storage_gb, ai_chat_count, ai_image_count, 
    is_active
FROM af_subscription_addons
ORDER BY id;

SELECT 
    id, uid, addon_id, addon_code, status,
    COUNT(*) as total_count
FROM af_user_addons
GROUP BY id, uid, addon_id, addon_code, status
ORDER BY id;

COMMIT;
