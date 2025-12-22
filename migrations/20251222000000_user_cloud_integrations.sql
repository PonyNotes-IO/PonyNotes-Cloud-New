-- Create table to store per-user cloud integration tokens and metadata
-- Stores tokens for third-party cloud providers (e.g., baidu)
CREATE TABLE IF NOT EXISTS user_cloud_integrations (
  id UUID NOT NULL DEFAULT gen_random_uuid(),
  provider VARCHAR(64) NOT NULL, -- e.g., 'baidu', 'dropbox'
  user_uid BIGINT NOT NULL REFERENCES af_user(uid) ON DELETE CASCADE,
  access_token TEXT,
  refresh_token TEXT,
  expires_at TIMESTAMP WITH TIME ZONE,
  scopes TEXT,
  meta JSONB DEFAULT '{}'::JSONB,
  is_revoked BOOLEAN NOT NULL DEFAULT FALSE,
  last_refreshed_at TIMESTAMP WITH TIME ZONE,
  created_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (id)
);

-- Ensure one integration record per user+provider by default (can be relaxed if multi-account per provider desired)
CREATE UNIQUE INDEX IF NOT EXISTS ux_user_provider ON user_cloud_integrations (user_uid, provider);
CREATE INDEX IF NOT EXISTS ix_user_cloud_integrations_provider ON user_cloud_integrations (provider);

-- Trigger to auto-update updated_at on row modification
CREATE OR REPLACE FUNCTION update_user_cloud_integrations_updated_at_func() RETURNS TRIGGER AS $$
BEGIN
  NEW.updated_at = NOW();
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE TRIGGER trg_update_user_cloud_integrations_modtime
BEFORE UPDATE ON user_cloud_integrations
FOR EACH ROW EXECUTE PROCEDURE update_user_cloud_integrations_updated_at_func();


