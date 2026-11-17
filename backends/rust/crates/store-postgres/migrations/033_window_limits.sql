-- Optional session + weekly usage windows on subscription credits (docs/WINDOW_LIMITS.md).
-- The limits are snapshotted on each entitlement part; the latest active part of a subscription
-- governs every bucket that subscription funds (an upgrade adds a part).
ALTER TABLE sr_entitlements ADD COLUMN window_limits jsonb;
-- One row per opened window. A window opens at the first reservation after the previous one ended;
-- spent/held are in the plan's asset and only ever change in the tenant transaction that also
-- changes the bucket, so they cannot drift from the ledger.
CREATE TABLE sr_usage_windows (
 workspace text NOT NULL, environment text NOT NULL, id text NOT NULL, subscription text NOT NULL,
 kind text NOT NULL CHECK(kind IN ('session','weekly')),
 window_start timestamptz NOT NULL, window_end timestamptz NOT NULL,
 spent numeric(38,0) NOT NULL DEFAULT 0 CHECK(spent>=0), held numeric(38,0) NOT NULL DEFAULT 0 CHECK(held>=0),
 PRIMARY KEY(workspace,environment,id), UNIQUE(workspace,environment,subscription,kind,window_start),
 FOREIGN KEY(workspace,environment,subscription) REFERENCES sr_subscriptions(workspace,environment,id),
 CHECK(window_end>window_start)
);
CREATE INDEX sr_usage_windows_current ON sr_usage_windows(workspace,environment,subscription,kind,window_end DESC);
-- Which windows a bucket allocation was counted against, so settlement, release and refund
-- adjust exactly those windows even after they have closed.
ALTER TABLE sr_allocations ADD COLUMN windows text[] NOT NULL DEFAULT '{}';
