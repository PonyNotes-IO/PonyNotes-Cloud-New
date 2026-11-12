-- Keep legacy workspace/custom KB IDs and permissions when importing the bridge.
ALTER TABLE af_weknora_sync_state
  ADD COLUMN IF NOT EXISTS settings JSONB NOT NULL DEFAULT '{}'::jsonb
  CHECK (jsonb_typeof(settings) = 'object');

INSERT INTO af_weknora_sync_state(singleton, state)
VALUES (true, '{"users":{},"workspaces":{}}'::jsonb)
ON CONFLICT(singleton) DO NOTHING;

UPDATE af_weknora_sync_state
SET state = '{"workspaces":{}}'::jsonb || state;
