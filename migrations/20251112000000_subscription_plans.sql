-- 会员计划表
CREATE TABLE IF NOT EXISTS af_subscription_plans (
    id SERIAL PRIMARY KEY,
    plan_code VARCHAR(50) UNIQUE NOT NULL, -- 计划代码: free_local, student, standard, team, enterprise
    plan_name VARCHAR(100) NOT NULL, -- 计划名称
    plan_name_cn VARCHAR(100) NOT NULL, -- 计划中文名称
    monthly_price_yuan DECIMAL(10, 2) NOT NULL DEFAULT 0, -- 月付价格，单位：元
    yearly_price_yuan DECIMAL(10, 2) NOT NULL DEFAULT 0, -- 年付价格，单位：元
    -- 基础功能
    cloud_storage_gb INTEGER DEFAULT -1, -- 云存储空间，GB，-1表示无，0表示无限制
    has_inbox BOOLEAN NOT NULL DEFAULT FALSE, -- 是否有收件箱
    has_multi_device_sync BOOLEAN NOT NULL DEFAULT FALSE, -- 是否支持多端同步
    has_api_support BOOLEAN NOT NULL DEFAULT FALSE, -- 是否支持API
    version_history_days INTEGER DEFAULT -1, -- 版本历史天数，-1表示无
    -- AI功能
    ai_chat_count_per_month INTEGER DEFAULT -1, -- AI对话次数/月，-1表示无限制
    ai_image_generation_per_month INTEGER DEFAULT -1, -- 图片生成次数/月，-1表示无限制
    -- 共享和协作
    has_share_link BOOLEAN NOT NULL DEFAULT FALSE, -- 是否有分享链接
    has_publish BOOLEAN NOT NULL DEFAULT FALSE, -- 是否有发布功能
    workspace_member_limit INTEGER DEFAULT -1, -- 工作区成员数量限制，-1表示无限制
    collaborative_workspace_limit INTEGER DEFAULT -1, -- 协作工作区数量限制，-1表示无限制，0表示仅限1个
    -- 安全与权限管理
    page_permission_guest_editors INTEGER DEFAULT -1, -- 页面权限访客编辑数量，-1表示无，0表示仅查看
    has_space_member_management BOOLEAN NOT NULL DEFAULT FALSE, -- 是否有空间成员管理
    has_space_member_grouping BOOLEAN NOT NULL DEFAULT FALSE, -- 是否有空间成员分组
    is_active BOOLEAN NOT NULL DEFAULT TRUE, -- 是否激活
    created_at TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP
);

-- 创建更新时间触发器
CREATE OR REPLACE FUNCTION update_af_subscription_plans_updated_at() RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = NOW();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER trigger_update_af_subscription_plans_updated_at
    BEFORE UPDATE ON af_subscription_plans
    FOR EACH ROW
    EXECUTE FUNCTION update_af_subscription_plans_updated_at();

-- 插入默认会员计划数据
INSERT INTO af_subscription_plans (
    plan_code, plan_name, plan_name_cn,
    monthly_price_yuan, yearly_price_yuan,
    cloud_storage_gb, has_inbox, has_multi_device_sync, has_api_support, version_history_days,
    ai_chat_count_per_month, ai_image_generation_per_month,
    has_share_link, has_publish, workspace_member_limit, collaborative_workspace_limit,
    page_permission_guest_editors, has_space_member_management, has_space_member_grouping
) VALUES
-- 免费版（本地）
('free_local', 'Free Local', '免费版（本地）',
    0, 0,
    -1, FALSE, FALSE, FALSE, -1,
    -1, -1,
    FALSE, FALSE, -1, -1,
    -1, FALSE, FALSE),
-- 学生版
('student', 'Student', '学生版',
    3, 30,
    2, TRUE, TRUE, TRUE, 7,
    10, -1,
    TRUE, TRUE, 2, 0,
    0, TRUE, FALSE),
-- 标准版
('standard', 'Standard', '标准版',
    8, 80,
    10, TRUE, TRUE, TRUE, 7,
    40, 10,
    TRUE, TRUE, 5, -1,
    10, TRUE, FALSE),
-- 团队版
('team', 'Team', '团队版',
    18, 180,
    20, TRUE, TRUE, TRUE, 30,
    120, 20,
    TRUE, TRUE, 10, -1,
    50, TRUE, TRUE),
-- 企业/校园版
('enterprise', 'Enterprise/Campus', '企业/校园版',
    0, 0,
    -1, TRUE, TRUE, TRUE, -1,
    -1, -1,
    TRUE, TRUE, -1, -1,
    -1, TRUE, TRUE)
ON CONFLICT (plan_code) DO NOTHING;

-- 用户订阅表
CREATE TABLE IF NOT EXISTS af_user_subscriptions (
    id SERIAL PRIMARY KEY,
    uid BIGINT NOT NULL REFERENCES af_user(uid) ON DELETE CASCADE,
    plan_id INTEGER NOT NULL REFERENCES af_subscription_plans(id) ON DELETE RESTRICT,
    billing_type VARCHAR(20) NOT NULL CHECK (billing_type IN ('monthly', 'yearly')), -- 计费类型
    status VARCHAR(20) NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'canceled', 'expired', 'pending')), -- 订阅状态
    start_date TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT CURRENT_TIMESTAMP, -- 开始日期
    end_date TIMESTAMP WITH TIME ZONE NOT NULL, -- 结束日期
    canceled_at TIMESTAMP WITH TIME ZONE DEFAULT NULL, -- 取消日期
    created_at TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP
);

-- 创建更新时间触发器
CREATE OR REPLACE FUNCTION update_af_user_subscriptions_updated_at() RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = NOW();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER trigger_update_af_user_subscriptions_updated_at
    BEFORE UPDATE ON af_user_subscriptions
    FOR EACH ROW
    EXECUTE FUNCTION update_af_user_subscriptions_updated_at();

-- 创建索引
CREATE INDEX IF NOT EXISTS idx_af_user_subscriptions_uid ON af_user_subscriptions(uid);
CREATE INDEX IF NOT EXISTS idx_af_user_subscriptions_plan_id ON af_user_subscriptions(plan_id);
CREATE INDEX IF NOT EXISTS idx_af_user_subscriptions_status ON af_user_subscriptions(status);
CREATE INDEX IF NOT EXISTS idx_af_user_subscriptions_end_date ON af_user_subscriptions(end_date);

-- 订阅补充包表
CREATE TABLE IF NOT EXISTS af_subscription_addons (
    id SERIAL PRIMARY KEY,
    addon_code VARCHAR(50) UNIQUE NOT NULL, -- 补充包代码
    addon_name VARCHAR(100) NOT NULL, -- 补充包名称
    addon_name_cn VARCHAR(100) NOT NULL, -- 补充包中文名称
    addon_type VARCHAR(20) NOT NULL CHECK (addon_type IN ('storage', 'ai_token')), -- 补充包类型
    price_yuan DECIMAL(10, 2) NOT NULL, -- 价格，单位：元
    -- 存储补充包字段
    storage_gb INTEGER DEFAULT NULL, -- 存储空间，GB，仅当addon_type=storage时有效
    -- AI Token补充包字段
    ai_chat_count INTEGER DEFAULT NULL, -- AI对话次数，仅当addon_type=ai_token时有效
    ai_image_count INTEGER DEFAULT NULL, -- AI图片生成次数，仅当addon_type=ai_token时有效
    is_active BOOLEAN NOT NULL DEFAULT TRUE, -- 是否激活
    created_at TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP
);

-- 创建更新时间触发器
CREATE OR REPLACE FUNCTION update_af_subscription_addons_updated_at() RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = NOW();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER trigger_update_af_subscription_addons_updated_at
    BEFORE UPDATE ON af_subscription_addons
    FOR EACH ROW
    EXECUTE FUNCTION update_af_subscription_addons_updated_at();

-- 插入默认补充包数据
INSERT INTO af_subscription_addons (
    addon_code, addon_name, addon_name_cn, addon_type, price_yuan,
    storage_gb, ai_chat_count, ai_image_count
) VALUES
-- 云存储空间补充包
('storage_5gb', 'Storage 5GB', '云存储空间 5GB', 'storage', 5, 5, NULL, NULL),
('storage_20gb', 'Storage 20GB', '云存储空间 20GB', 'storage', 15, 20, NULL, NULL),
('storage_50gb', 'Storage 50GB', '云存储空间 50GB', 'storage', 35, 50, NULL, NULL),
-- AI Token补充包
('ai_token_100', 'AI Token 100', 'AI Token 100次10张', 'ai_token', 10, NULL, 100, 10),
('ai_token_400', 'AI Token 400', 'AI Token 400次20张', 'ai_token', 30, NULL, 400, 20),
('ai_token_1000', 'AI Token 1000', 'AI Token 1000次50张', 'ai_token', 50, NULL, 1000, 50)
ON CONFLICT (addon_code) DO NOTHING;

-- 用户补充包表
CREATE TABLE IF NOT EXISTS af_user_addons (
    id SERIAL PRIMARY KEY,
    uid BIGINT NOT NULL REFERENCES af_user(uid) ON DELETE CASCADE,
    addon_id INTEGER NOT NULL REFERENCES af_subscription_addons(id) ON DELETE RESTRICT,
    quantity INTEGER NOT NULL DEFAULT 1, -- 购买数量
    start_date TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT CURRENT_TIMESTAMP, -- 开始日期
    end_date TIMESTAMP WITH TIME ZONE NOT NULL, -- 结束日期（对于存储补充包，通常是永久有效，但可以设置过期时间）
    status VARCHAR(20) NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'expired', 'used')), -- 状态
    created_at TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP
);

-- 创建更新时间触发器
CREATE OR REPLACE FUNCTION update_af_user_addons_updated_at() RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = NOW();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER trigger_update_af_user_addons_updated_at
    BEFORE UPDATE ON af_user_addons
    FOR EACH ROW
    EXECUTE FUNCTION update_af_user_addons_updated_at();

-- 创建索引
CREATE INDEX IF NOT EXISTS idx_af_user_addons_uid ON af_user_addons(uid);
CREATE INDEX IF NOT EXISTS idx_af_user_addons_addon_id ON af_user_addons(addon_id);
CREATE INDEX IF NOT EXISTS idx_af_user_addons_status ON af_user_addons(status);
CREATE INDEX IF NOT EXISTS idx_af_user_addons_end_date ON af_user_addons(end_date);

-- 用户订阅使用记录表（用于记录AI使用情况等）
CREATE TABLE IF NOT EXISTS af_user_subscription_usage (
    id SERIAL PRIMARY KEY,
    uid BIGINT NOT NULL REFERENCES af_user(uid) ON DELETE CASCADE,
    subscription_id INTEGER REFERENCES af_user_subscriptions(id) ON DELETE SET NULL,
    usage_date DATE NOT NULL, -- 使用日期
    usage_type VARCHAR(50) NOT NULL, -- 使用类型: ai_chat, ai_image, storage_bytes
    usage_count BIGINT NOT NULL DEFAULT 0, -- 使用次数或使用量
    created_at TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP
);

-- 创建索引
CREATE INDEX IF NOT EXISTS idx_af_user_subscription_usage_uid ON af_user_subscription_usage(uid);
CREATE INDEX IF NOT EXISTS idx_af_user_subscription_usage_date ON af_user_subscription_usage(usage_date);
CREATE INDEX IF NOT EXISTS idx_af_user_subscription_usage_type ON af_user_subscription_usage(usage_type);
CREATE UNIQUE INDEX IF NOT EXISTS idx_af_user_subscription_usage_unique ON af_user_subscription_usage(uid, usage_date, usage_type);

