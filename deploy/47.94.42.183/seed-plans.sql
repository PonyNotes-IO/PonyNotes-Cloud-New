-- The original seed predates the plan codes used by current quota checks.
-- cloud_storage_gb stores MB after the 20260204010000 migration.
INSERT INTO af_subscription_plans (
    plan_code, plan_name, plan_name_cn, monthly_price_yuan, yearly_price_yuan,
    cloud_storage_gb, has_inbox, has_multi_device_sync, has_api_support,
    version_history_days, ai_chat_count_per_month, ai_image_generation_per_month,
    has_share_link, has_publish, workspace_member_limit, collaborative_workspace_limit,
    page_permission_guest_editors, has_space_member_management, has_space_member_grouping
) VALUES
('mfb', 'Free', '免费版', 0, 0, 300, true, true, false, 7, 20, 0, true, true, 1, 2, 0, false, false),
('profersor', 'Professional', '专业版', 15, 158, 51200, true, true, true, 30, 1200, 10, true, true, 5, 10, 10, true, false),
('hiclass', 'Advanced', '高级版', 29, 298, 153600, true, true, true, 30, 3000, 20, true, true, 10, 18, 50, true, true)
ON CONFLICT (plan_code) DO NOTHING;
