-- Remove social login fields from af_user table (revert 20260402000000)
-- These fields are no longer needed as OAuth identity is stored in auth.users + auth.identities

DROP INDEX IF EXISTS af_user_wechat_openid_key;
DROP INDEX IF EXISTS af_user_douyin_openid_key;
DROP INDEX IF EXISTS af_user_bind_mobile_idx;

ALTER TABLE af_user DROP COLUMN IF EXISTS wechat_openid;
ALTER TABLE af_user DROP COLUMN IF EXISTS douyin_openid;
ALTER TABLE af_user DROP COLUMN IF EXISTS bind_mobile;
