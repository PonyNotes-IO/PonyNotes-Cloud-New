-- Standalone Agent conversations keep their identity outside the note folder tree.
CREATE TABLE af_agent_conversations (
  conversation_id UUID PRIMARY KEY,
  uid BIGINT NOT NULL REFERENCES af_user(uid) ON DELETE CASCADE,
  workspace_id UUID NOT NULL REFERENCES af_workspace(workspace_id) ON DELETE CASCADE,
  title TEXT NOT NULL,
  artifacts JSONB NOT NULL DEFAULT '{}',
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX af_agent_conversations_owner ON af_agent_conversations(uid, workspace_id, updated_at DESC);
ALTER TABLE af_agent_conversations ENABLE ROW LEVEL SECURITY;
