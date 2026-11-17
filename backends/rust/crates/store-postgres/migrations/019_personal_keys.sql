-- Personal API keys: a wallet user calls the API directly and pays from their own wallet.
-- The key is an ordinary key in the wallet workspace (customer = the user); this table
-- records which user owns it and the name and cap they gave it.
CREATE TABLE sr_personal_keys (
 workspace text NOT NULL, environment text NOT NULL, key_id text NOT NULL,
 user_id text NOT NULL REFERENCES sr_users(id), name text NOT NULL,
 cap jsonb, created_at timestamptz NOT NULL DEFAULT now(), revoked_at timestamptz,
 PRIMARY KEY(workspace,environment,key_id),
 FOREIGN KEY(workspace,environment,key_id) REFERENCES sr_keys(workspace,environment,id)
);
CREATE INDEX sr_personal_keys_user ON sr_personal_keys(user_id, created_at);
-- Per-key usage (personal keys, app statistics) looks requests up by budget lineage.
CREATE INDEX sr_requests_lineage_time ON sr_requests(key_lineage, created_at) WHERE key_lineage IS NOT NULL;
