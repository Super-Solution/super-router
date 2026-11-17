-- Single-use, hashed email tokens: verification (24h) and password reset (1h).
CREATE TABLE sr_user_tokens (
 id text PRIMARY KEY, user_id text NOT NULL REFERENCES sr_users(id),
 kind text NOT NULL CHECK(kind IN ('verify_email','reset_password')), token_hash text NOT NULL UNIQUE,
 email text NOT NULL, created_at timestamptz NOT NULL DEFAULT now(), expires_at timestamptz NOT NULL, used_at timestamptz
);
CREATE INDEX sr_user_tokens_user ON sr_user_tokens(user_id,kind,created_at);
-- TOTP: seeds are encrypted with the host secret; last_step blocks code replay.
ALTER TABLE sr_users ADD COLUMN totp_secret text;
ALTER TABLE sr_users ADD COLUMN totp_pending text;
ALTER TABLE sr_users ADD COLUMN totp_enabled_at timestamptz;
ALTER TABLE sr_users ADD COLUMN totp_last_step bigint;
ALTER TABLE sr_users ADD COLUMN recovery_codes text[];
-- Second-factor step after a correct first factor; short-lived, single use, attempt-limited.
CREATE TABLE sr_mfa_challenges (
 id text PRIMARY KEY, token_hash text NOT NULL UNIQUE, user_id text NOT NULL REFERENCES sr_users(id),
 method text NOT NULL, ip text, user_agent text, created_at timestamptz NOT NULL, expires_at timestamptz NOT NULL,
 attempts integer NOT NULL DEFAULT 0, used_at timestamptz
);
