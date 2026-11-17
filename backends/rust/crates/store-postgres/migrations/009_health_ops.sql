-- Shared health: every node adds its attempt outcomes to minute buckets (additive upserts).
CREATE TABLE sr_health_minutes (
 workspace text NOT NULL, environment text NOT NULL, supply_key text NOT NULL, bucket timestamptz NOT NULL,
 successes integer NOT NULL DEFAULT 0 CHECK(successes>=0), failures integer NOT NULL DEFAULT 0 CHECK(failures>=0),
 unknown integer NOT NULL DEFAULT 0 CHECK(unknown>=0), latency_hist integer[] NOT NULL,
 PRIMARY KEY(workspace,environment,supply_key,bucket)
);
CREATE INDEX sr_health_minutes_bucket ON sr_health_minutes(bucket);
-- One incident row per supply key; evaluated by the leader worker only, from shared buckets.
CREATE TABLE sr_health_alerts (
 workspace text NOT NULL, environment text NOT NULL, supply_key text NOT NULL,
 state text NOT NULL DEFAULT 'ok' CHECK(state IN ('ok','open')), seq integer NOT NULL DEFAULT 0,
 opened_at timestamptz, closed_at timestamptz, last_down_notice timestamptz, notified boolean NOT NULL DEFAULT false,
 PRIMARY KEY(workspace,environment,supply_key)
);
-- Automatic disable on credential/account failures, with the offering snapshot needed to probe it later.
ALTER TABLE sr_disabled_connections ADD COLUMN auto boolean NOT NULL DEFAULT false;
ALTER TABLE sr_disabled_connections ADD COLUMN reason text;
ALTER TABLE sr_disabled_connections ADD COLUMN failure_code text;
ALTER TABLE sr_disabled_connections ADD COLUMN offering_config jsonb;
ALTER TABLE sr_disabled_connections ADD COLUMN disabled_at timestamptz NOT NULL DEFAULT now();
ALTER TABLE sr_disabled_connections ADD COLUMN next_probe_at timestamptz;
-- System events (health, connections) are not tied to a request.
ALTER TABLE sr_outbox ALTER COLUMN request DROP NOT NULL;
-- Leader leases for singleton jobs; generation increases on every change of holder.
CREATE TABLE sr_leases (
 name text PRIMARY KEY, holder text NOT NULL, generation bigint NOT NULL DEFAULT 1, expires_at timestamptz NOT NULL
);
