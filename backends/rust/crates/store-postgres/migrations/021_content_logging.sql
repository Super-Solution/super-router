-- Policies store no request content. An explicit Service policy overrides its project.
ALTER TABLE sr_requests ADD COLUMN content_policy jsonb;
CREATE TABLE sr_content_policies (
 workspace text NOT NULL, environment text NOT NULL, kind text NOT NULL CHECK(kind IN ('service','project')),
 target text NOT NULL, policy jsonb NOT NULL, PRIMARY KEY(workspace,environment,kind,target)
);
CREATE TABLE sr_request_content (
 workspace text NOT NULL, environment text NOT NULL, request text NOT NULL,
 payload text NOT NULL, encrypted boolean NOT NULL, expires_at timestamptz NOT NULL,
 PRIMARY KEY(workspace,environment,request),
 FOREIGN KEY(workspace,environment,request) REFERENCES sr_requests(workspace,environment,id)
);
CREATE INDEX sr_content_expiry ON sr_request_content(expires_at);
