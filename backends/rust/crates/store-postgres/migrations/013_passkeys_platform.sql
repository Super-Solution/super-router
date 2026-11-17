-- Passkeys (WebAuthn credentials). The credential id is globally unique by spec.
CREATE TABLE sr_passkeys (
 id text PRIMARY KEY, user_id text NOT NULL REFERENCES sr_users(id), public_key jsonb NOT NULL, alg integer NOT NULL,
 sign_count bigint NOT NULL DEFAULT 0, transports text[], name text, backed_up boolean NOT NULL DEFAULT false,
 created_at timestamptz NOT NULL DEFAULT now(), last_used_at timestamptz
);
CREATE INDEX sr_passkeys_user ON sr_passkeys(user_id);
CREATE TABLE sr_webauthn_challenges (
 challenge_hash text PRIMARY KEY, user_id text REFERENCES sr_users(id),
 kind text NOT NULL CHECK(kind IN ('register','login')), expires_at timestamptz NOT NULL, used_at timestamptz
);
-- Platform operators (set by the host) manage every workspace; tenants never can.
ALTER TABLE sr_users ADD COLUMN platform_role text CHECK(platform_role IN ('operator'));
ALTER TABLE sr_workspaces ADD COLUMN name text;
ALTER TABLE sr_workspaces ADD COLUMN created_by text REFERENCES sr_users(id);
CREATE INDEX sr_workspaces_creator ON sr_workspaces(created_by);
