-- sub2api owns balances and usage logs. Never expose its keys through client APIs.
CREATE TABLE af_agent_accounts (
    uid BIGINT PRIMARY KEY REFERENCES af_user(uid) ON DELETE CASCADE,
    sub2api_user_id BIGINT NOT NULL UNIQUE CHECK (sub2api_user_id > 0),
    sub2api_key_id BIGINT NOT NULL UNIQUE CHECK (sub2api_key_id > 0)
);
ALTER TABLE af_agent_accounts ENABLE ROW LEVEL SECURITY;
