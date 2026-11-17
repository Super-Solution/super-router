-- Developer payouts through Stripe Connect.
CREATE TABLE sr_payout_accounts (
 user_id text PRIMARY KEY REFERENCES sr_users(id), provider text NOT NULL CHECK(provider='stripe'),
 account_id text NOT NULL UNIQUE, payouts_enabled boolean NOT NULL DEFAULT false,
 details_submitted boolean NOT NULL DEFAULT false,
 created_at timestamptz NOT NULL DEFAULT now(), updated_at timestamptz NOT NULL DEFAULT now()
);
-- One payout pays a fixed set of earnings rows (they point back with payout_id). The
-- transfer is retried under `idempotency_key` while unknown; a definite rejection moves
-- to a new attempt (new key) only by operator action.
CREATE TABLE sr_payouts (
 id text PRIMARY KEY, user_id text NOT NULL REFERENCES sr_users(id), account_id text NOT NULL,
 asset text NOT NULL, credits numeric(38,0) NOT NULL CHECK(credits>0),
 currency text NOT NULL CHECK(currency ~ '^[A-Z]{3}$'), amount_minor numeric(38,0) NOT NULL CHECK(amount_minor>0),
 state text NOT NULL CHECK(state IN ('pending','paid','unknown','rejected')),
 attempt integer NOT NULL DEFAULT 1, idempotency_key text NOT NULL,
 transfer_id text, error text,
 created_at timestamptz NOT NULL DEFAULT now(), updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX sr_payouts_user ON sr_payouts(user_id, created_at);
CREATE INDEX sr_payouts_open ON sr_payouts(state) WHERE state IN ('pending','unknown');
ALTER TABLE sr_developer_earnings ADD CONSTRAINT sr_developer_earnings_payout_fk
 FOREIGN KEY(payout_id) REFERENCES sr_payouts(id);
