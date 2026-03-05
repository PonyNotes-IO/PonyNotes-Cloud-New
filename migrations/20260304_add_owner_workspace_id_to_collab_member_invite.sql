-- 添加 owner_workspace_id 字段到 af_collab_member_invite 表
-- 用于记录被分享视图所属的原始 workspace_id（即分享者的 workspace_id）
-- 解决网格类视图（Grid/Board/Calendar）分享后被邀请者客户端无法确定正确 workspace_id 的问题
-- 根本原因：af_collab 表只存储 database_id 的条目，没有 view_id 的条目
-- 所以 list_shared_views_handler 通过 af_collab WHERE oid=view_id 查询会失败

ALTER TABLE af_collab_member_invite
ADD COLUMN IF NOT EXISTS owner_workspace_id UUID;

COMMENT ON COLUMN af_collab_member_invite.owner_workspace_id IS '分享者的工作区ID（view所属的原始workspace）';
