-- Business versions are created only by an explicit user action.
UPDATE af_subscription_plans
SET version_history_days = CASE
  WHEN plan_code IN ('standard', 'stand') THEN 7
  WHEN plan_code IN ('hiclass', 'profersor') THEN 30
  ELSE 0
END;
COMMENT ON COLUMN af_subscription_plans.version_history_days IS 'Manual document history retention in days; 0 or negative disables history';

CREATE TABLE af_document_version (
  snapshot_id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
  workspace_id UUID NOT NULL REFERENCES af_workspace(workspace_id) ON DELETE CASCADE,
  oid UUID NOT NULL,
  created_by BIGINT REFERENCES af_user(uid) ON DELETE SET NULL,
  document BYTEA NOT NULL,
  created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
  expires_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX af_document_version_list ON af_document_version(workspace_id, oid, created_at DESC, snapshot_id DESC);
CREATE INDEX af_document_version_expiry ON af_document_version(expires_at);
