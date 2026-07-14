-- 通知归档状态（账号级全局通知改造）：
-- 归档此前只存在于客户端各工作区的 user-awareness 副本中，切换工作区/换设备后丢失。
-- 通知的唯一权威源是 af_notification（按 recipient_uid），归档状态同样收敛到服务端，
-- 使任意工作区/设备看到一致的归档结果。
ALTER TABLE af_notification
  ADD COLUMN IF NOT EXISTS is_archived BOOLEAN NOT NULL DEFAULT FALSE,
  ADD COLUMN IF NOT EXISTS archived_at TIMESTAMPTZ;
