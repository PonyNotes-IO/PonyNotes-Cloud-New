-- 会员升级暂存：升级到更高付费套餐时，原低级套餐转 pending 并暂停计时，
-- 用 remaining_seconds 记录暂停时的剩余时长（秒）。高级套餐自然到期且无 active 时，
-- 逐级恢复等级最高的 pending：start_date=NOW()，end_date=NOW()+remaining_seconds，随后置空。
ALTER TABLE af_user_subscriptions ADD COLUMN IF NOT EXISTS remaining_seconds BIGINT;

COMMENT ON COLUMN af_user_subscriptions.remaining_seconds IS
  '升级到高级套餐时暂停(pending)的低级套餐剩余时长(秒)。恢复时 end_date=NOW()+remaining_seconds，随后置 NULL。';
