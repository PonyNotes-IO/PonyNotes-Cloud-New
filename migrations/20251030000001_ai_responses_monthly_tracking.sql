-- Add columns to track AI conversation responses per month
-- This supports the new subscription plan limits for AI usage

ALTER TABLE af_workspace_ai_usage 
ADD COLUMN IF NOT EXISTS ai_responses_count INT DEFAULT 0,
ADD COLUMN IF NOT EXISTS ai_image_responses_count INT DEFAULT 0;

-- Create an index for faster monthly queries
CREATE INDEX IF NOT EXISTS idx_af_workspace_ai_usage_workspace_month 
ON af_workspace_ai_usage(workspace_id, created_at);

-- Add a comment to document the new columns
COMMENT ON COLUMN af_workspace_ai_usage.ai_responses_count IS 'Number of AI text responses (conversations) used in this day';
COMMENT ON COLUMN af_workspace_ai_usage.ai_image_responses_count IS 'Number of AI image generations used in this day';

