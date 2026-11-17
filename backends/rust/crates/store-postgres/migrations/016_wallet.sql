-- "Sign in with SuperRouter": apps spend from a user's wallet with the user's consent.
-- Apps are OAuth clients owned by a user account; secrets are stored hashed.
CREATE TABLE sr_oauth_clients (
 id text PRIMARY KEY, name text NOT NULL, owner_user text NOT NULL REFERENCES sr_users(id),
 homepage text, redirect_uris text[] NOT NULL CHECK(cardinality(redirect_uris) BETWEEN 1 AND 10),
 service_ids text[] NOT NULL CHECK(cardinality(service_ids) BETWEEN 1 AND 50),
 secret_hash text, created_at timestamptz NOT NULL DEFAULT now(), disabled_at timestamptz
);
CREATE INDEX sr_oauth_clients_owner ON sr_oauth_clients(owner_user);
-- Single-use authorization codes bound to the PKCE challenge, redirect URI and the cap the user chose.
CREATE TABLE sr_oauth_codes (
 code_hash text PRIMARY KEY, client_id text NOT NULL REFERENCES sr_oauth_clients(id),
 user_id text NOT NULL REFERENCES sr_users(id), redirect_uri text NOT NULL,
 code_challenge text NOT NULL, cap jsonb NOT NULL,
 expires_at timestamptz NOT NULL, used_at timestamptz
);
-- One grant per consent. The access token is an ordinary API key in the wallet workspace
-- (customer = the user's wallet, spend limit = the cap); refresh rotates it inside the
-- same budget lineage, so the cap keeps counting across tokens.
CREATE TABLE sr_oauth_grants (
 id text PRIMARY KEY, client_id text NOT NULL REFERENCES sr_oauth_clients(id),
 user_id text NOT NULL REFERENCES sr_users(id),
 workspace text NOT NULL, environment text NOT NULL, customer text NOT NULL,
 key_id text NOT NULL, budget_id text NOT NULL,
 refresh_hash text NOT NULL UNIQUE, previous_refresh_hash text,
 cap jsonb NOT NULL, created_at timestamptz NOT NULL DEFAULT now(),
 refreshed_at timestamptz, revoked_at timestamptz, revoked_reason text,
 FOREIGN KEY(workspace,environment,customer) REFERENCES sr_customers(workspace,environment,id)
);
CREATE INDEX sr_oauth_grants_user ON sr_oauth_grants(user_id, created_at);
CREATE INDEX sr_oauth_grants_previous ON sr_oauth_grants(previous_refresh_hash);
