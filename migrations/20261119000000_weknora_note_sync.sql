CREATE TABLE af_weknora_note_sync (
  workspace_id uuid NOT NULL REFERENCES af_workspace(workspace_id) ON DELETE CASCADE,
  uid bigint NOT NULL REFERENCES af_user(uid) ON DELETE CASCADE,
  source_id uuid NOT NULL,
  run_id uuid NOT NULL,
  started_at timestamptz NOT NULL DEFAULT now(),
  result jsonb,
  PRIMARY KEY (workspace_id, uid, source_id)
);
