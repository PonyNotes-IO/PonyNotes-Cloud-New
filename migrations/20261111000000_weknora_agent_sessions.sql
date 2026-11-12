-- Conversation identities only; credentials remain in Redis / WeKnora secret storage.
CREATE TABLE af_weknora_agent_sessions (
  uid BIGINT NOT NULL REFERENCES af_user(uid) ON DELETE CASCADE,
  workspace_id UUID NOT NULL REFERENCES af_workspace(workspace_id) ON DELETE CASCADE,
  document_id UUID NOT NULL,
  state JSONB NOT NULL DEFAULT '{}',
  PRIMARY KEY (uid, workspace_id, document_id)
);
ALTER TABLE af_weknora_agent_sessions ENABLE ROW LEVEL SECURITY;
