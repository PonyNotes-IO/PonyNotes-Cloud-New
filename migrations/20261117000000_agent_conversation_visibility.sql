-- Existing conversations remain private, including all retained Agent sessions.
ALTER TABLE af_agent_conversations
  ADD COLUMN visibility TEXT NOT NULL DEFAULT 'private'
  CHECK (visibility IN ('private', 'shared'));

CREATE INDEX af_agent_conversations_shared
  ON af_agent_conversations(workspace_id, updated_at DESC, conversation_id)
  WHERE visibility = 'shared';
