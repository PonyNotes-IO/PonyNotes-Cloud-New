-- 通知已读状态：af_notification 原本只有 processed(已投递)，没有真正的"已读"。
-- 导致客户端标记已读无处持久化、刷新后又变未读；且补发按 processed 过滤会永久漏掉
-- "已投递但客户端没接住"的通知。这里引入 is_read/read_at，作为账号级已读的唯一真相。
ALTER TABLE af_notification ADD COLUMN IF NOT EXISTS is_read BOOLEAN NOT NULL DEFAULT false;
ALTER TABLE af_notification ADD COLUMN IF NOT EXISTS read_at TIMESTAMPTZ;

-- 按收件人 + 未读 + 时间的复合索引，加速"拉取用户通知/补发未读"。
CREATE INDEX IF NOT EXISTS idx_af_notification_recipient_unread
  ON af_notification (recipient_uid, is_read, created_at DESC);
