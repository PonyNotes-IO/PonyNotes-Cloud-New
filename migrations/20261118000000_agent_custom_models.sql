CREATE TABLE af_agent_custom_models (
  id UUID PRIMARY KEY,
  workspace_id UUID NOT NULL REFERENCES af_workspace(workspace_id) ON DELETE CASCADE,
  creator_uid BIGINT NOT NULL REFERENCES af_user(uid) ON DELETE CASCADE,
  config JSONB NOT NULL CHECK (jsonb_typeof(config) = 'object'),
  credential BYTEA NOT NULL,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX af_agent_custom_models_workspace ON af_agent_custom_models(workspace_id);
CREATE INDEX af_agent_custom_models_creator ON af_agent_custom_models(creator_uid);
