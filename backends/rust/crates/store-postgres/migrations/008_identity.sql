-- People, not just API keys. Accounts live in this database; sign-in is a local password,
-- an OIDC identity, or both. Members of a workspace may be bound to a user.
CREATE TABLE sr_users (
 id text PRIMARY KEY, email text, email_verified boolean NOT NULL DEFAULT false, display_name text,
 password_hash text, status text NOT NULL DEFAULT 'active' CHECK(status IN ('active','disabled')),
 -- Bumped on password change or disable: every session issued under an older version dies.
 auth_version integer NOT NULL DEFAULT 1 CHECK(auth_version>0),
 failed_logins integer NOT NULL DEFAULT 0 CHECK(failed_logins>=0), locked_until timestamptz,
 created_at timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX sr_users_email ON sr_users(lower(email)) WHERE email IS NOT NULL;
CREATE TABLE sr_user_identities (
 issuer text NOT NULL, subject text NOT NULL, provider text NOT NULL,
 user_id text NOT NULL REFERENCES sr_users(id), email text, created_at timestamptz NOT NULL DEFAULT now(),
 PRIMARY KEY(issuer,subject)
);
CREATE INDEX sr_user_identities_user ON sr_user_identities(user_id);
CREATE TABLE sr_sessions (
 id text PRIMARY KEY, token_hash text NOT NULL UNIQUE, user_id text NOT NULL REFERENCES sr_users(id),
 auth_version integer NOT NULL, method text NOT NULL, ip text, user_agent text,
 created_at timestamptz NOT NULL, last_seen_at timestamptz NOT NULL, expires_at timestamptz NOT NULL, revoked_at timestamptz
);
CREATE INDEX sr_sessions_user ON sr_sessions(user_id,created_at);
-- Single-use OIDC login/link state. Holds the nonce and PKCE verifier; consumed atomically.
CREATE TABLE sr_oidc_states (
 state_hash text PRIMARY KEY, provider text NOT NULL, nonce text NOT NULL, code_verifier text NOT NULL,
 intent text NOT NULL CHECK(intent IN ('login','link')), user_id text REFERENCES sr_users(id),
 expires_at timestamptz NOT NULL, used_at timestamptz
);
ALTER TABLE sr_members ADD COLUMN user_id text REFERENCES sr_users(id);
CREATE UNIQUE INDEX sr_members_user ON sr_members(workspace,environment,user_id) WHERE user_id IS NOT NULL;
-- An invitation may be restricted to one verified email; accepted invitations record the user.
ALTER TABLE sr_invitations ADD COLUMN email text;
ALTER TABLE sr_invitations ADD COLUMN user_id text REFERENCES sr_users(id);
