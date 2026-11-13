-- WeKnora automations are scoped to its own tenant, which has no workspace
-- concept. This table gives every automation an owning workspace so listings
-- never leak across workspaces of the same user.
CREATE TABLE af_agent_automations (
  automation_id TEXT PRIMARY KEY,
  uid BIGINT NOT NULL REFERENCES af_user(uid) ON DELETE CASCADE,
  workspace_id UUID NOT NULL REFERENCES af_workspace(workspace_id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  model_id TEXT,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX af_agent_automations_owner
    ON af_agent_automations(uid, workspace_id, created_at DESC);
ALTER TABLE af_agent_automations ENABLE ROW LEVEL SECURITY;
