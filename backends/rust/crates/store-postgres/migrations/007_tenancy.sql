-- Workspace entity: a tenant has a lifecycle of its own instead of being an implicit text key.
CREATE TABLE sr_workspaces (
 workspace text PRIMARY KEY, status text NOT NULL DEFAULT 'active' CHECK(status IN ('active','suspended')),
 reason text, created_at timestamptz NOT NULL DEFAULT now(), updated_at timestamptz NOT NULL DEFAULT now()
);
INSERT INTO sr_workspaces(workspace) SELECT DISTINCT workspace FROM sr_members ON CONFLICT DO NOTHING;

-- Keys: issue time anchors spend windows; budget_id is the key lineage so rotation never resets a cap.
ALTER TABLE sr_keys ADD COLUMN created_at timestamptz NOT NULL DEFAULT now();
ALTER TABLE sr_keys ADD COLUMN budget_id text;
UPDATE sr_keys SET budget_id=id WHERE budget_id IS NULL;
ALTER TABLE sr_keys ALTER COLUMN budget_id SET NOT NULL;
ALTER TABLE sr_keys ADD COLUMN budget_anchor timestamptz;
UPDATE sr_keys SET budget_anchor=created_at WHERE budget_anchor IS NULL;
ALTER TABLE sr_keys ALTER COLUMN budget_anchor SET NOT NULL;
ALTER TABLE sr_keys ADD COLUMN spend_limit jsonb;
ALTER TABLE sr_keys ADD COLUMN allow_ips text[];

-- One row per key lineage and window. A new window is a new row, so there is no reset job to race.
CREATE TABLE sr_key_spend (
 workspace text NOT NULL, environment text NOT NULL, budget_id text NOT NULL, window_start timestamptz NOT NULL,
 asset text NOT NULL, held numeric(38,0) NOT NULL DEFAULT 0 CHECK(held>=0), spent numeric(38,0) NOT NULL DEFAULT 0 CHECK(spent>=0),
 PRIMARY KEY(workspace,environment,budget_id,window_start)
);
ALTER TABLE sr_requests ADD COLUMN key_budget text;
ALTER TABLE sr_requests ADD COLUMN key_window timestamptz;

ALTER TABLE sr_customers ADD COLUMN segment text;

CREATE TABLE sr_invitations (
 workspace text NOT NULL, environment text NOT NULL, id text NOT NULL, member_id text NOT NULL,
 role text NOT NULL CHECK(role IN ('owner','service_admin','billing_admin','viewer','app')),
 customer text NOT NULL, scopes text[] NOT NULL, service_ids text[], key_expires_at timestamptz NOT NULL,
 spend_limit jsonb, allow_ips text[], token_hash text NOT NULL UNIQUE,
 state text NOT NULL DEFAULT 'pending' CHECK(state IN ('pending','accepted','declined','revoked')),
 expires_at timestamptz NOT NULL, created_by text NOT NULL, created_at timestamptz NOT NULL DEFAULT now(),
 handled_at timestamptz, key_id text,
 PRIMARY KEY(workspace,environment,id)
);
-- At most one live invitation per prospective member; enforced by the database, not a count check.
CREATE UNIQUE INDEX sr_invitation_pending ON sr_invitations(workspace,environment,member_id) WHERE state='pending';
