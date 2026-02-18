-- 修改 af_collab_member_invite 表，使 received_uid 可以为 NULL
-- 分享链接创建时还没有接收者，received_uid 应为 NULL，接受邀请后才填入

-- 1. 删除原主键约束（因为主键列不能为NULL）
ALTER TABLE af_collab_member_invite DROP CONSTRAINT IF EXISTS af_collab_member_invite_pk;

-- 2. 删除 received_uid 上的外键约束
ALTER TABLE af_collab_member_invite DROP CONSTRAINT IF EXISTS af_collab_member_invite_received_uid_fkey;

-- 3. 将 received_uid 改为可空
ALTER TABLE af_collab_member_invite ALTER COLUMN received_uid DROP NOT NULL;

-- 4. 重新添加外键约束（允许NULL值）
ALTER TABLE af_collab_member_invite 
ADD CONSTRAINT af_collab_member_invite_received_uid_fkey 
FOREIGN KEY (received_uid) REFERENCES af_user(uid) ON DELETE CASCADE;

-- 5. 添加新的唯一约束：每个文档每个发送者对每个接收者只能有一条记录
-- 对于 received_uid 非空的记录
CREATE UNIQUE INDEX IF NOT EXISTS idx_collab_member_invite_unique_with_receiver
ON af_collab_member_invite(oid, send_uid, received_uid) WHERE received_uid IS NOT NULL;

-- 对于 received_uid 为空的记录（分享链接），每个文档每个发送者只能有一条待接受记录
CREATE UNIQUE INDEX IF NOT EXISTS idx_collab_member_invite_unique_pending
ON af_collab_member_invite(oid, send_uid) WHERE received_uid IS NULL;

COMMENT ON COLUMN af_collab_member_invite.received_uid IS '被邀请者的用户ID，分享链接创建时为NULL，接受邀请后填入';


