-- Fixed one-minute windows shared by every node. Scopes: key lineage, workspace, auth address.
CREATE TABLE sr_rate_windows (
 scope text NOT NULL, id text NOT NULL, window_start timestamptz NOT NULL,
 requests integer NOT NULL DEFAULT 0 CHECK(requests>=0), tokens bigint NOT NULL DEFAULT 0 CHECK(tokens>=0),
 PRIMARY KEY(scope,id,window_start)
);
CREATE INDEX sr_rate_windows_start ON sr_rate_windows(window_start);
ALTER TABLE sr_keys ADD COLUMN rate_limit jsonb;
ALTER TABLE sr_invitations ADD COLUMN rate_limit jsonb;
ALTER TABLE sr_workspaces ADD COLUMN rate_limit jsonb;
-- What a request was admitted against, so settlement can true-up token estimates.
ALTER TABLE sr_requests ADD COLUMN key_lineage text;
ALTER TABLE sr_requests ADD COLUMN rate_window timestamptz;
ALTER TABLE sr_requests ADD COLUMN rate_tokens bigint;
ALTER TABLE sr_requests ADD COLUMN rate_scopes text[];
CREATE INDEX sr_requests_inflight_key ON sr_requests(key_lineage) WHERE billing='reserved' AND execution IN ('accepted','running');
CREATE INDEX sr_requests_inflight_ws ON sr_requests(workspace) WHERE billing='reserved' AND execution IN ('accepted','running');
