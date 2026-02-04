-- 2026-02-04-更新订阅计划套餐数据
-- 根据最新标准更新订阅计划：
-- 免费版：云空间300MB，工作区2个，AI对话20次
-- 标准版：月付9元年付99元，云空间10GB，工作区5个，AI对话300次
-- 专业版：月付15元年付158元，云空间50GB，工作区10个，AI对话1200次
-- 高级版：月付29元年付298元，云空间150GB，工作区18个，AI对话3000次

-- 更新免费版 (mfb)
UPDATE af_subscription_plans SET
    monthly_price_yuan = 0,
    yearly_price_yuan = 0,
    cloud_storage_gb = 0,  -- 300MB，在GB单位下显示为0
    ai_chat_count_per_month = 20,
    collaborative_workspace_limit = 2
WHERE plan_code = 'mfb';

-- 更新标准版 (standard)
UPDATE af_subscription_plans SET
    monthly_price_yuan = 9,
    yearly_price_yuan = 99,
    cloud_storage_gb = 10,
    ai_chat_count_per_month = 300,
    collaborative_workspace_limit = 5
WHERE plan_code = 'standard';

-- 更新专业版 (profersor)
UPDATE af_subscription_plans SET
    monthly_price_yuan = 15,
    yearly_price_yuan = 158,
    cloud_storage_gb = 50,
    ai_chat_count_per_month = 1200,
    collaborative_workspace_limit = 10
WHERE plan_code = 'profersor';

-- 更新高级版 (hiclass)
UPDATE af_subscription_plans SET
    monthly_price_yuan = 29,
    yearly_price_yuan = 298,
    cloud_storage_gb = 150,
    ai_chat_count_per_month = 3000,
    collaborative_workspace_limit = 18
WHERE plan_code = 'hiclass';

-- 将重复的标准版记录标记为非活跃 (stand)
UPDATE af_subscription_plans SET is_active = FALSE WHERE plan_code = 'stand';

-- 验证结果
SELECT
    plan_code,
    plan_name_cn,
    monthly_price_yuan,
    yearly_price_yuan,
    cloud_storage_gb,
    ai_chat_count_per_month,
    collaborative_workspace_limit,
    is_active
FROM af_subscription_plans
WHERE plan_code IN ('mfb', 'standard', 'profersor', 'hiclass')
ORDER BY id;
