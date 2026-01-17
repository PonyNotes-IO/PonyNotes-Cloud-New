-- Create join_requests table for space join request functionality
CREATE TABLE IF NOT EXISTS join_requests (
    id UUID PRIMARY KEY,
    workspace_id UUID NOT NULL,
    space_id UUID NOT NULL,
    requester_id BIGINT NOT NULL,
    reason TEXT,
    status TEXT NOT NULL DEFAULT 'pending',
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    FOREIGN KEY (workspace_id) REFERENCES af_workspace(workspace_id) ON DELETE CASCADE,
    FOREIGN KEY (requester_id) REFERENCES af_user(uid) ON DELETE CASCADE
);

-- Create indexes for better query performance
CREATE INDEX IF NOT EXISTS idx_join_requests_workspace_space ON join_requests(workspace_id, space_id);
CREATE INDEX IF NOT EXISTS idx_join_requests_requester ON join_requests(requester_id);
CREATE INDEX IF NOT EXISTS idx_join_requests_status ON join_requests(status);
-- Ensure only one pending request per user per space
CREATE UNIQUE INDEX IF NOT EXISTS idx_join_requests_unique_pending ON join_requests(space_id, requester_id) WHERE status = 'pending';
