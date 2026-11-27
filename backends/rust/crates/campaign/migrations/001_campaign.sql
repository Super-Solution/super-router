-- Campaign extension tables. Every time is epoch milliseconds, every amount an integer in the host's own grant unit.
-- No customer table is referenced: a customer is whatever opaque id the host passes in.

CREATE TABLE sc_settings (
 id boolean PRIMARY KEY DEFAULT true CHECK (id),
 referral_enabled boolean NOT NULL DEFAULT true,
 hold_days integer NOT NULL DEFAULT 7 CHECK (hold_days BETWEEN 0 AND 90),
 attach_window_hours integer NOT NULL DEFAULT 72 CHECK (attach_window_hours BETWEEN 1 AND 8760),
 rebate_window_days integer CHECK (rebate_window_days BETWEEN 1 AND 3650),
 rebate_cap bigint CHECK (rebate_cap > 0),
 max_per_signal integer NOT NULL DEFAULT 3 CHECK (max_per_signal BETWEEN 1 AND 1000),
 code_fail_limit integer NOT NULL DEFAULT 8 CHECK (code_fail_limit BETWEEN 1 AND 100),
 code_fail_window_minutes integer NOT NULL DEFAULT 60 CHECK (code_fail_window_minutes BETWEEN 1 AND 1440),
 updated_at bigint NOT NULL DEFAULT 0
);
INSERT INTO sc_settings DEFAULT VALUES;

CREATE TABLE sc_rate_classes (
 name text PRIMARY KEY CHECK (name ~ '^[a-z][a-z0-9_]{0,31}$'),
 rate_bps integer NOT NULL CHECK (rate_bps BETWEEN 0 AND 10000),
 is_default boolean NOT NULL DEFAULT false,
 updated_at bigint NOT NULL DEFAULT 0
);
CREATE UNIQUE INDEX sc_rate_classes_one_default ON sc_rate_classes ((true)) WHERE is_default;
INSERT INTO sc_rate_classes (name, rate_bps, is_default) VALUES ('default', 300, true);

CREATE TABLE sc_referrers (
 customer text PRIMARY KEY,
 code text NOT NULL UNIQUE CHECK (code ~ '^[0-9A-Z]{8}$'),
 class text NOT NULL REFERENCES sc_rate_classes (name) ON UPDATE CASCADE ON DELETE RESTRICT,
 created_at bigint NOT NULL
);

CREATE TABLE sc_referrals (
 referred text PRIMARY KEY,
 referrer text NOT NULL REFERENCES sc_referrers (customer),
 state text NOT NULL CHECK (state IN ('active', 'flagged', 'void')),
 reason text,
 attached_at bigint NOT NULL,
 reviewed_at bigint,
 CHECK (referred <> referrer)
);
CREATE INDEX sc_referrals_referrer ON sc_referrals (referrer, attached_at);

-- Keyed hashes of what the host reports about a customer (a network prefix, a normalised address, a device id).
CREATE TABLE sc_signals (
 customer text NOT NULL,
 signal bytea NOT NULL,
 seen_at bigint NOT NULL,
 PRIMARY KEY (customer, signal)
);
CREATE INDEX sc_signals_signal ON sc_signals (signal);

CREATE TABLE sc_rebates (
 reference text PRIMARY KEY,
 referrer text NOT NULL,
 referred text NOT NULL,
 base bigint NOT NULL CHECK (base > 0),
 rate_bps integer NOT NULL CHECK (rate_bps BETWEEN 1 AND 10000),
 amount bigint NOT NULL CHECK (amount > 0),
 state text NOT NULL CHECK (state IN ('held', 'released', 'void')),
 release_at bigint NOT NULL,
 created_at bigint NOT NULL,
 settled_at bigint,
 reason text
);
CREATE INDEX sc_rebates_due ON sc_rebates (release_at) WHERE state = 'held';
CREATE INDEX sc_rebates_referrer ON sc_rebates (referrer, created_at);
CREATE INDEX sc_rebates_referred ON sc_rebates (referred) WHERE state = 'held';

CREATE TABLE sc_batches (
 id text PRIMARY KEY,
 name text NOT NULL CHECK (char_length(name) BETWEEN 1 AND 80),
 kind text NOT NULL CHECK (kind IN ('serial', 'custom')),
 amount bigint NOT NULL CHECK (amount > 0),
 max_uses integer NOT NULL CHECK (max_uses >= 1),
 redeem_until bigint,
 grant_days integer CHECK (grant_days BETWEEN 1 AND 3650),
 code_count integer NOT NULL CHECK (code_count >= 1),
 created_by text,
 created_at bigint NOT NULL,
 disabled_at bigint
);

CREATE TABLE sc_codes (
 id text PRIMARY KEY,
 batch text NOT NULL REFERENCES sc_batches (id),
 hash bytea NOT NULL UNIQUE,
 hint text NOT NULL,
 used integer NOT NULL DEFAULT 0 CHECK (used >= 0),
 disabled_at bigint
);
CREATE INDEX sc_codes_batch ON sc_codes (batch);

CREATE TABLE sc_redemptions (
 id text PRIMARY KEY,
 code text NOT NULL REFERENCES sc_codes (id),
 batch text NOT NULL REFERENCES sc_batches (id),
 customer text NOT NULL,
 amount bigint NOT NULL CHECK (amount > 0),
 grant_key text NOT NULL UNIQUE,
 network bytea,
 at bigint NOT NULL,
 UNIQUE (code, customer)
);
CREATE INDEX sc_redemptions_at ON sc_redemptions (at DESC);
CREATE INDEX sc_redemptions_customer ON sc_redemptions (customer, at DESC);

-- Failed guesses only, so a customer or a network that tries too many unknown codes is locked out for a while.
CREATE TABLE sc_attempts (
 id bigserial PRIMARY KEY,
 customer text NOT NULL,
 network bytea,
 at bigint NOT NULL
);
CREATE INDEX sc_attempts_customer ON sc_attempts (customer, at);
CREATE INDEX sc_attempts_network ON sc_attempts (network, at) WHERE network IS NOT NULL;
