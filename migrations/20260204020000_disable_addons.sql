-- 2026-02-04-禁用附加包(addon)相关表
-- 由于业务调整，附加包功能已移除，此迁移将禁用 addon 相关数据

-- 开始事务
BEGIN;

-- Step 1: 将所有附加包标记为非活跃
UPDATE af_subscription_addons SET is_active = FALSE;

-- Step 2: 将所有用户附加包标记为已过期
UPDATE af_user_addons SET status = 'expired';

-- 验证结果 - af_subscription_addons
SELECT
    id, addon_code, addon_name, addon_type,
    price_yuan, storage_gb, ai_chat_count, ai_image_count,
    is_active
FROM af_subscription_addons
ORDER BY id;

-- 验证结果 - af_user_addons
SELECT
    id, uid, addon_id, quantity, status,
    COUNT(*) as total_count
FROM af_user_addons
GROUP BY id, uid, addon_id, quantity, status
ORDER BY id;

COMMIT;
