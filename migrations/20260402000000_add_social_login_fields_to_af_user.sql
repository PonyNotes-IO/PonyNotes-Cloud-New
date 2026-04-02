-- Add social login fields to af_user table
-- wechat_openid: WeChat user openid for WeChat login
-- douyin_openid: Douyin (TikTok) user openid for Douyin login
-- bind_mobile: Phone number bound via social login
--   - For old phones (already in af_user.phone for another user): only stored in bind_mobile
--   - For new phones: stored in both phone and bind_mobile

ALTER TABLE af_user ADD COLUMN IF NOT EXISTS wechat_openid TEXT DEFAULT NULL;
ALTER TABLE af_user ADD COLUMN IF NOT EXISTS douyin_openid TEXT DEFAULT NULL;
ALTER TABLE af_user ADD COLUMN IF NOT EXISTS bind_mobile TEXT DEFAULT NULL;

-- Unique indexes for openids (each openid can only belong to one user)
CREATE UNIQUE INDEX IF NOT EXISTS af_user_wechat_openid_key
  ON af_user (wechat_openid)
  WHERE wechat_openid IS NOT NULL;

CREATE UNIQUE INDEX IF NOT EXISTS af_user_douyin_openid_key
  ON af_user (douyin_openid)
  WHERE douyin_openid IS NOT NULL;

-- Index for bind_mobile lookups
-- NOT unique: same phone can appear in bind_mobile for SSO user AND phone for phone user
CREATE INDEX IF NOT EXISTS af_user_bind_mobile_idx
  ON af_user (bind_mobile)
  WHERE bind_mobile IS NOT NULL;

COMMENT ON COLUMN af_user.wechat_openid IS 'WeChat user openid, unique, set during WeChat login';
COMMENT ON COLUMN af_user.douyin_openid IS 'Douyin (TikTok) user openid, unique, set during Douyin login';
COMMENT ON COLUMN af_user.bind_mobile IS 'Phone number bound via social login. For new phones: same as phone field. For old phones (already taken): different from phone field (which stays NULL). Not unique.';
