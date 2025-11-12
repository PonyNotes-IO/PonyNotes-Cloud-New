-- Migration: Update subscription plans from Pro/Team to Basic/Pro/Team
-- Date: 2025-10-30
-- Description: Migrate existing Pro users to Pro plan and update Team plan value

-- IMPORTANT: This migration changes the enum values for subscription plans
-- Old: Free=0, Pro=1, Team=2, AiMax=3, AiLocal=4
-- New: Free=0, Basic=1, Pro=2, Team=3, AiMax=4, AiLocal=5

BEGIN;

-- Step 1: Update af_workspace table
-- Migrate Pro (workspace_type=1) to Pro (workspace_type=2)
-- Migrate Team (workspace_type=2) to Team (workspace_type=3)
UPDATE af_workspace 
SET workspace_type = 
  CASE 
    WHEN workspace_type = 2 THEN 3  -- Team: 2 -> 3
    WHEN workspace_type = 1 THEN 2  -- Pro: 1 -> 2 (Pro)
    ELSE workspace_type
  END
WHERE workspace_type IN (1, 2);

-- Step 2: If you have a subscription_plans table or similar, update it
-- (Adjust table name based on your actual schema)
-- Example:
-- UPDATE subscription_records 
-- SET plan_type = 
--   CASE 
--     WHEN plan_type = 2 THEN 3  -- Team
--     WHEN plan_type = 1 THEN 2  -- Pro to Pro
--     WHEN plan_type = 3 THEN 4  -- AiMax
--     WHEN plan_type = 4 THEN 5  -- AiLocal
--     ELSE plan_type
--   END
-- WHERE plan_type IN (1, 2, 3, 4);

-- Step 3: Add comments for clarity
COMMENT ON COLUMN af_workspace.workspace_type IS 'Workspace subscription type: 0=Free, 1=Basic, 2=Pro, 3=Team';

-- Verify migration
DO $$
DECLARE
  free_count INTEGER;
  basic_count INTEGER;
  pro_count INTEGER;
  team_count INTEGER;
BEGIN
  SELECT COUNT(*) INTO free_count FROM af_workspace WHERE workspace_type = 0;
  SELECT COUNT(*) INTO basic_count FROM af_workspace WHERE workspace_type = 1;
  SELECT COUNT(*) INTO pro_count FROM af_workspace WHERE workspace_type = 2;
  SELECT COUNT(*) INTO team_count FROM af_workspace WHERE workspace_type = 3;
  
  RAISE NOTICE 'Migration completed:';
  RAISE NOTICE 'Free workspaces: %', free_count;
  RAISE NOTICE 'Basic workspaces: %', basic_count;
  RAISE NOTICE 'Pro workspaces: %', pro_count;
  RAISE NOTICE 'Team workspaces: %', team_count;
END $$;

COMMIT;

-- Rollback script (if needed):
-- BEGIN;
-- UPDATE af_workspace 
-- SET workspace_type = 
--   CASE 
--     WHEN workspace_type = 3 THEN 2  -- Team: 3 -> 2
--     WHEN workspace_type = 2 THEN 1  -- Pro: 2 -> 1 (Pro)
--     ELSE workspace_type
--   END
-- WHERE workspace_type IN (2, 3);
-- COMMIT;

