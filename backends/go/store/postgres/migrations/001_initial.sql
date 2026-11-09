CREATE TABLE IF NOT EXISTS sr_services (
 workspace text NOT NULL, environment text NOT NULL, id text NOT NULL, version integer NOT NULL CHECK(version>0),
 config jsonb NOT NULL, active boolean NOT NULL DEFAULT true, PRIMARY KEY(workspace,environment,id,version)
);
CREATE UNIQUE INDEX IF NOT EXISTS sr_service_active ON sr_services(workspace,environment,id) WHERE active;
CREATE TABLE IF NOT EXISTS sr_accounts (
 workspace text NOT NULL, environment text NOT NULL, id text NOT NULL, owner text NOT NULL, kind text NOT NULL,
 asset text NOT NULL, balance numeric(38,0) NOT NULL DEFAULT 0 CHECK(balance>=0), held numeric(38,0) NOT NULL DEFAULT 0 CHECK(held>=0 AND held<=balance),
 PRIMARY KEY(workspace,environment,id), UNIQUE(workspace,environment,owner,kind,asset)
);
CREATE TABLE IF NOT EXISTS sr_buckets (
 workspace text NOT NULL, environment text NOT NULL, id text NOT NULL, account text NOT NULL, source text NOT NULL,
 expires_at timestamptz, remaining numeric(38,0) NOT NULL CHECK(remaining>=0), held numeric(38,0) NOT NULL DEFAULT 0 CHECK(held>=0 AND held<=remaining),
 PRIMARY KEY(workspace,environment,id), UNIQUE(workspace,environment,source),
 FOREIGN KEY(workspace,environment,account) REFERENCES sr_accounts(workspace,environment,id)
);
CREATE TABLE IF NOT EXISTS sr_requests (
 workspace text NOT NULL, environment text NOT NULL, id text NOT NULL, customer text NOT NULL, idem text NOT NULL, digest text NOT NULL,
 service jsonb NOT NULL, legs jsonb NOT NULL, execution text NOT NULL DEFAULT 'accepted', billing text NOT NULL DEFAULT 'reserved',
 cancel_requested boolean NOT NULL DEFAULT false, output_started boolean NOT NULL DEFAULT false,
 provider_spent numeric(38,0) NOT NULL DEFAULT 0 CHECK(provider_spent>=0), provider_held numeric(38,0) NOT NULL DEFAULT 0 CHECK(provider_held>=0),
 deadline timestamptz NOT NULL, created_at timestamptz NOT NULL DEFAULT now(),
 PRIMARY KEY(workspace,environment,id), UNIQUE(workspace,environment,idem)
);
CREATE TABLE IF NOT EXISTS sr_allocations (
 workspace text NOT NULL, environment text NOT NULL, request text NOT NULL, bucket text NOT NULL, account text NOT NULL,
 amount numeric(38,0) NOT NULL CHECK(amount>=0), PRIMARY KEY(workspace,environment,request,bucket),
 FOREIGN KEY(workspace,environment,request) REFERENCES sr_requests(workspace,environment,id),
 FOREIGN KEY(workspace,environment,bucket) REFERENCES sr_buckets(workspace,environment,id),
 FOREIGN KEY(workspace,environment,account) REFERENCES sr_accounts(workspace,environment,id)
);
CREATE TABLE IF NOT EXISTS sr_attempts (
 workspace text NOT NULL, environment text NOT NULL, id text NOT NULL, request text NOT NULL, offering text NOT NULL,
 status text NOT NULL DEFAULT 'dispatched', cost_bound numeric(38,0) NOT NULL, cost numeric(38,0), usage jsonb, upstream_id text,
 created_at timestamptz NOT NULL DEFAULT now(), PRIMARY KEY(workspace,environment,id),
 FOREIGN KEY(workspace,environment,request) REFERENCES sr_requests(workspace,environment,id)
);
CREATE TABLE IF NOT EXISTS sr_disabled_connections (
 workspace text NOT NULL, environment text NOT NULL, offering text NOT NULL, PRIMARY KEY(workspace,environment,offering)
);
CREATE TABLE IF NOT EXISTS sr_settlements (
 workspace text NOT NULL, environment text NOT NULL, request text NOT NULL, kind text NOT NULL, account text NOT NULL, amount numeric(38,0) NOT NULL CHECK(amount>=0),
 PRIMARY KEY(workspace,environment,request,kind), FOREIGN KEY(workspace,environment,request) REFERENCES sr_requests(workspace,environment,id),
 FOREIGN KEY(workspace,environment,account) REFERENCES sr_accounts(workspace,environment,id)
);
-- A transfer encodes BOTH sides in one immutable row; equal amounts by construction.
-- Counterpart names are not spendable accounts and may hold issuer/consumed balances.
CREATE TABLE IF NOT EXISTS sr_journal (
 workspace text NOT NULL, environment text NOT NULL, business_key text NOT NULL, asset text NOT NULL,
 debit text NOT NULL, credit text NOT NULL, amount numeric(38,0) NOT NULL CHECK(amount>=0), reason text NOT NULL,
 created_at timestamptz NOT NULL DEFAULT now(), PRIMARY KEY(workspace,environment,business_key), CHECK(debit<>credit)
);
CREATE OR REPLACE FUNCTION sr_immutable() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'append-only record'; END $$;
DROP TRIGGER IF EXISTS sr_journal_immutable ON sr_journal;
CREATE TRIGGER sr_journal_immutable BEFORE UPDATE OR DELETE ON sr_journal FOR EACH ROW EXECUTE FUNCTION sr_immutable();
CREATE TABLE IF NOT EXISTS sr_outbox (
 id text PRIMARY KEY, workspace text NOT NULL, environment text NOT NULL, request text NOT NULL, payload jsonb NOT NULL,
 state text NOT NULL DEFAULT 'pending', generation integer NOT NULL DEFAULT 0, lease_until timestamptz,
 created_at timestamptz NOT NULL DEFAULT now(), UNIQUE(workspace,environment,request,payload),
 FOREIGN KEY(workspace,environment,request) REFERENCES sr_requests(workspace,environment,id)
);
CREATE TABLE IF NOT EXISTS sr_reconciliation (
 workspace text NOT NULL, environment text NOT NULL, request text NOT NULL, reason text NOT NULL,
 state text NOT NULL DEFAULT 'open', resolution text, resolved_at timestamptz,
 PRIMARY KEY(workspace,environment,request), FOREIGN KEY(workspace,environment,request) REFERENCES sr_requests(workspace,environment,id)
);
CREATE INDEX IF NOT EXISTS sr_requests_pending ON sr_requests(deadline) WHERE billing IN ('reserved','pending');
CREATE INDEX IF NOT EXISTS sr_outbox_pending ON sr_outbox(state,lease_until,created_at);
