ALTER TABLE af_document_version
  ADD COLUMN source TEXT CHECK (source IN ('automatic', 'manual', 'restore_backup'));

CREATE TABLE af_document_version_pending (
  workspace_id UUID NOT NULL REFERENCES af_workspace(workspace_id) ON DELETE CASCADE,
  oid UUID NOT NULL,
  first_edit_ms BIGINT,
  last_edit_ms BIGINT,
  last_edit_seq INTEGER NOT NULL DEFAULT 0,
  covered_edit_ms BIGINT NOT NULL DEFAULT 0,
  covered_edit_seq INTEGER NOT NULL DEFAULT 0,
  retry_at TIMESTAMPTZ NOT NULL DEFAULT '-infinity',
  storage_limited BOOLEAN NOT NULL DEFAULT FALSE,
  due_ms BIGINT GENERATED ALWAYS AS (
    LEAST(first_edit_ms + 600000, last_edit_ms + 120000)
  ) STORED,
  PRIMARY KEY (workspace_id, oid)
);
CREATE INDEX af_document_version_pending_due ON af_document_version_pending(due_ms)
  WHERE due_ms IS NOT NULL;

COMMENT ON COLUMN af_subscription_plans.version_history_days
  IS 'Document history retention in days; 0 or negative disables history';
