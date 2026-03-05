-- 添加 view_layout 字段到 af_collab_member_invite 表
-- 用于记录被分享视图的布局类型（Document=0, Grid=1, Board=2, Calendar=3, Chat=4）
-- 解决网格类视图分享后被邀请者无法打开的问题

ALTER TABLE af_collab_member_invite
ADD COLUMN IF NOT EXISTS view_layout INTEGER NOT NULL DEFAULT 0;

COMMENT ON COLUMN af_collab_member_invite.view_layout IS '视图布局类型：0=文档，1=网格，2=看板，3=日历，4=聊天';
