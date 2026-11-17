CREATE TABLE sr_tasks (
 workspace text NOT NULL, environment text NOT NULL, id text NOT NULL,
 principal jsonb NOT NULL, input jsonb NOT NULL,
 status text NOT NULL DEFAULT 'queued', completed_units jsonb NOT NULL DEFAULT '{}',
 cancel_requested boolean NOT NULL DEFAULT false,
 generation integer NOT NULL DEFAULT 0, lease_until timestamptz, due_at timestamptz NOT NULL DEFAULT now(),
 polls integer NOT NULL DEFAULT 0,
 PRIMARY KEY(workspace,environment,id),
 FOREIGN KEY(workspace,environment,id) REFERENCES sr_requests(workspace,environment,id)
);
CREATE INDEX sr_tasks_due ON sr_tasks(due_at,lease_until) WHERE status IN ('queued','running');
CREATE TABLE sr_task_steps (
 workspace text NOT NULL, environment text NOT NULL, task text NOT NULL, id text NOT NULL,
 attempt text NOT NULL, status text NOT NULL DEFAULT 'running', external_id text,
 operation_id text NOT NULL, completed_units jsonb NOT NULL DEFAULT '{}', output jsonb,
 PRIMARY KEY(workspace,environment,task,id), UNIQUE(workspace,environment,task,operation_id),
 FOREIGN KEY(workspace,environment,task) REFERENCES sr_tasks(workspace,environment,id),
 FOREIGN KEY(workspace,environment,attempt) REFERENCES sr_attempts(workspace,environment,id)
);
CREATE TABLE sr_task_commands (
 workspace text NOT NULL, environment text NOT NULL, task text NOT NULL, idem text NOT NULL,
 digest text NOT NULL, created_at timestamptz NOT NULL DEFAULT now(),
 PRIMARY KEY(workspace,environment,task,idem),
 FOREIGN KEY(workspace,environment,task) REFERENCES sr_tasks(workspace,environment,id)
);
CREATE TRIGGER sr_task_commands_immutable BEFORE UPDATE OR DELETE ON sr_task_commands FOR EACH ROW EXECUTE FUNCTION sr_immutable();
