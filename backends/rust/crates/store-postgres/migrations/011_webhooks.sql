-- Customer webhook endpoints. Signing secrets are AES-256-GCM encrypted with a host key;
-- a rotated secret stays valid (and is also signed with) until its expiry.
CREATE TABLE sr_webhook_endpoints (
 workspace text NOT NULL, environment text NOT NULL, id text NOT NULL, url text NOT NULL,
 event_types text[], description text, secrets jsonb NOT NULL,
 state text NOT NULL DEFAULT 'active' CHECK(state IN ('active','disabled')), disabled_reason text,
 created_at timestamptz NOT NULL DEFAULT now(),
 PRIMARY KEY(workspace,environment,id)
);
CREATE TABLE sr_webhook_deliveries (
 id text PRIMARY KEY, workspace text NOT NULL, environment text NOT NULL, endpoint text NOT NULL,
 event_id text NOT NULL, payload jsonb NOT NULL,
 state text NOT NULL DEFAULT 'pending' CHECK(state IN ('pending','delivered','dead')),
 attempts integer NOT NULL DEFAULT 0, next_attempt_at timestamptz NOT NULL DEFAULT now(),
 last_status integer, last_error text, created_at timestamptz NOT NULL DEFAULT now(), delivered_at timestamptz,
 UNIQUE(workspace,environment,endpoint,event_id),
 FOREIGN KEY(workspace,environment,endpoint) REFERENCES sr_webhook_endpoints(workspace,environment,id)
);
CREATE INDEX sr_webhook_due ON sr_webhook_deliveries(next_attempt_at) WHERE state='pending';
-- Permanent payment-notification problems: acknowledged to the provider (stop redelivery),
-- kept here for operators. Deduplicated; an acknowledged anomaly stays acknowledged on replay.
CREATE TABLE sr_payment_anomalies (
 id text PRIMARY KEY, workspace text NOT NULL, environment text NOT NULL, integration text NOT NULL,
 reference text NOT NULL, code text NOT NULL, occurrences integer NOT NULL DEFAULT 1,
 first_seen timestamptz NOT NULL DEFAULT now(), last_seen timestamptz NOT NULL DEFAULT now(),
 acknowledged_at timestamptz, acknowledged_by text
);
CREATE INDEX sr_payment_anomalies_open ON sr_payment_anomalies(workspace,environment) WHERE acknowledged_at IS NULL;
