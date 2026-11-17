CREATE TABLE sr_provider_connections (
  id text PRIMARY KEY,
  workspace text,
  kind text NOT NULL CHECK (kind IN ('openai','anthropic','openai-compatible')),
  base_url text,
  region text NOT NULL,
  labels jsonb NOT NULL DEFAULT '[]',
  credential_enc text NOT NULL,
  key_prefix text NOT NULL,
  fingerprint text NOT NULL,
  created_at timestamptz NOT NULL DEFAULT now(),
  rotated_at timestamptz NOT NULL DEFAULT now(),
  status text NOT NULL DEFAULT 'active' CHECK (status IN ('active','disabled','deleted')),
  disabled_reason text,
  CHECK ((kind='openai-compatible') = (base_url IS NOT NULL))
);
CREATE INDEX sr_provider_connections_owner ON sr_provider_connections(workspace, status);
CREATE TABLE sr_provider_connection_revision (
  singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
  revision bigint NOT NULL DEFAULT 0
);
INSERT INTO sr_provider_connection_revision DEFAULT VALUES;
