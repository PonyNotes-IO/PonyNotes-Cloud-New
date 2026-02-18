-- 添加权限字段到 af_collab_member_invite 表
-- 用于记录分享链接的权限设置
ALTER TABLE af_collab_member_invite 
ADD COLUMN IF NOT EXISTS permission_id INTEGER NOT NULL DEFAULT 1;

-- 添加注释
COMMENT ON COLUMN af_collab_member_invite.permission_id IS '分享链接权限：1=查看，2=评论，3=编辑，4=全部权限';

-- 创建索引以加快查询
CREATE INDEX IF NOT EXISTS idx_collab_member_invite_received_uid 
ON af_collab_member_invite(received_uid);

-- 添加唯一约束（如果还没有的话）
-- 注意：这个约束意味着每个文档对每个接收者只能有一条邀请记录
