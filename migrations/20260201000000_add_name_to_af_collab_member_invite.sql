-- 添加 name 字段到 af_collab_member_invite 表，用于存储视图名称
-- 解决获取别人分享的笔记时权限不足导致标题显示"加载中..."的问题

ALTER TABLE af_collab_member_invite
    ADD COLUMN IF NOT EXISTS name TEXT NOT NULL DEFAULT '';

COMMENT ON COLUMN af_collab_member_invite.name IS '视图/笔记名称';
