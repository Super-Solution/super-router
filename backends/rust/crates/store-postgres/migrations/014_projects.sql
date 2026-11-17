-- Projects: app boundaries inside a workspace (across its environments). Services belong to
-- a project; members and keys may be restricted to projects. Customers and balances stay
-- workspace-wide so one end user can use several apps with one balance.
CREATE TABLE sr_projects (
 workspace text NOT NULL, id text NOT NULL, name text NOT NULL,
 created_at timestamptz NOT NULL DEFAULT now(), archived_at timestamptz,
 PRIMARY KEY(workspace,id)
);
ALTER TABLE sr_members ADD COLUMN projects text[];
ALTER TABLE sr_keys ADD COLUMN project_ids text[];
ALTER TABLE sr_invitations ADD COLUMN projects text[];
CREATE INDEX sr_requests_project ON sr_requests(workspace,environment,(service->>'project'));
