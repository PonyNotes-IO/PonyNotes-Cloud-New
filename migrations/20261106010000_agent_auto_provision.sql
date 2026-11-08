-- A committed provisioning identity lets retries recover external users/keys
-- after a timeout, even if the transaction saving their IDs did not commit.
ALTER TABLE af_agent_accounts
    ALTER COLUMN sub2api_user_id DROP NOT NULL,
    ALTER COLUMN sub2api_key_id DROP NOT NULL,
    ADD COLUMN provision_id UUID NOT NULL DEFAULT uuid_generate_v4() UNIQUE,
    ADD CONSTRAINT af_agent_account_complete
        CHECK ((sub2api_user_id IS NULL) = (sub2api_key_id IS NULL));
