-- Imported once from the private legacy sync/state.json before enabling the Worker.
-- Keep the original tenant, knowledge-base IDs and model configuration intact.
CREATE TABLE IF NOT EXISTS af_weknora_sync_state (
  singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
  state JSONB NOT NULL CHECK (jsonb_typeof(state -> 'users') = 'object'),
  last_run JSONB,
  updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
