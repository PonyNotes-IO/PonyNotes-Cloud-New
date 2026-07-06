-- 过期会员资源清理改为逻辑删除: af_blob_metadata 增加 deleted_at 标记。
-- 打上标记即视为"已删除"(不计用量、禁止下载), 对象存储文件保留, 可恢复。
-- af_workspace 已自带 deleted_at 字段, 无需新增。
ALTER TABLE af_blob_metadata ADD COLUMN IF NOT EXISTS deleted_at TIMESTAMP WITH TIME ZONE DEFAULT NULL;

-- 会员升级"暂存(pending)/到期恢复"机制按新规则废弃(升级即直接废弃低级别套餐):
-- 存量 pending 记录直接作废
UPDATE af_user_subscriptions
SET status = 'canceled', canceled_at = NOW(), cancel_reason = '系统: 升级暂存机制按新规则废弃'
WHERE status = 'pending';
