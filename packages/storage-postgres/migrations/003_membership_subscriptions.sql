CREATE TABLE sr_members (
 workspace text NOT NULL, environment text NOT NULL, id text NOT NULL, role text NOT NULL CHECK(role IN ('owner','service_admin','billing_admin','viewer','app')),
 active boolean NOT NULL DEFAULT true, PRIMARY KEY(workspace,environment,id)
);
CREATE TABLE sr_customers (
 workspace text NOT NULL, environment text NOT NULL, id text NOT NULL, external_id text NOT NULL,
 PRIMARY KEY(workspace,environment,id), UNIQUE(workspace,environment,external_id)
);
CREATE TABLE sr_keys (
 workspace text NOT NULL, environment text NOT NULL, id text NOT NULL, member_id text NOT NULL,
 customer text NOT NULL, hash text NOT NULL UNIQUE, scopes text[] NOT NULL, service_ids text[], expires_at timestamptz NOT NULL, revoked_at timestamptz,
 PRIMARY KEY(workspace,environment,id), FOREIGN KEY(workspace,environment,member_id) REFERENCES sr_members(workspace,environment,id)
);
CREATE TABLE sr_commands (
 workspace text NOT NULL, environment text NOT NULL, operation text NOT NULL, key text NOT NULL, digest text NOT NULL, result jsonb NOT NULL,
 PRIMARY KEY(workspace,environment,operation,key)
);
CREATE TABLE sr_audit (
 id text PRIMARY KEY, workspace text NOT NULL, environment text NOT NULL, actor text NOT NULL, action text NOT NULL, resource text NOT NULL,
 details jsonb NOT NULL DEFAULT '{}', created_at timestamptz NOT NULL DEFAULT now()
);
CREATE TRIGGER sr_audit_immutable BEFORE UPDATE OR DELETE ON sr_audit FOR EACH ROW EXECUTE FUNCTION sr_immutable();
CREATE TABLE sr_plans (
 workspace text NOT NULL, environment text NOT NULL, id text NOT NULL, version integer NOT NULL CHECK(version>0), config jsonb NOT NULL,
 PRIMARY KEY(workspace,environment,id,version)
);
CREATE TRIGGER sr_plan_immutable BEFORE UPDATE OR DELETE ON sr_plans FOR EACH ROW EXECUTE FUNCTION sr_immutable();
CREATE TABLE sr_subscriptions (
 workspace text NOT NULL, environment text NOT NULL, id text NOT NULL, customer text NOT NULL, plan_id text NOT NULL, plan_version integer NOT NULL,
 next_plan_id text, next_plan_version integer, anchor_at timestamptz NOT NULL, period_days integer NOT NULL CHECK(period_days>0),
 status text NOT NULL DEFAULT 'pending' CHECK(status IN ('pending','active','past_due','cancelled')),
 paid_cycle integer NOT NULL DEFAULT -1, current_period_end timestamptz, cancel_effective_at timestamptz,
 PRIMARY KEY(workspace,environment,id),
 FOREIGN KEY(workspace,environment,customer) REFERENCES sr_customers(workspace,environment,id),
 FOREIGN KEY(workspace,environment,plan_id,plan_version) REFERENCES sr_plans(workspace,environment,id,version),
 FOREIGN KEY(workspace,environment,next_plan_id,next_plan_version) REFERENCES sr_plans(workspace,environment,id,version)
);
CREATE TABLE sr_payment_orders (
 workspace text NOT NULL, environment text NOT NULL, id text NOT NULL, subscription text NOT NULL, cycle integer NOT NULL CHECK(cycle>=0),
 integration text NOT NULL, amount_minor numeric(38,0) NOT NULL CHECK(amount_minor>0), currency text NOT NULL, plan jsonb NOT NULL,
 period_start timestamptz NOT NULL, period_end timestamptz NOT NULL, revision integer NOT NULL DEFAULT 0,
 state text NOT NULL DEFAULT 'pending' CHECK(state IN ('pending','failed','succeeded','review_required')),
 PRIMARY KEY(workspace,environment,id), UNIQUE(workspace,environment,subscription,cycle),
 FOREIGN KEY(workspace,environment,subscription) REFERENCES sr_subscriptions(workspace,environment,id), CHECK(period_end>period_start)
);
CREATE TABLE sr_payment_events (
 integration text NOT NULL, event_id text NOT NULL, workspace text NOT NULL, environment text NOT NULL, order_id text NOT NULL,
 digest text NOT NULL, outcome text NOT NULL, created_at timestamptz NOT NULL DEFAULT now(), PRIMARY KEY(integration,event_id),
 FOREIGN KEY(workspace,environment,order_id) REFERENCES sr_payment_orders(workspace,environment,id)
);
CREATE TRIGGER sr_payment_event_immutable BEFORE UPDATE OR DELETE ON sr_payment_events FOR EACH ROW EXECUTE FUNCTION sr_immutable();
CREATE TABLE sr_entitlements (
 workspace text NOT NULL, environment text NOT NULL, subscription text NOT NULL, cycle integer NOT NULL, customer text NOT NULL,
 service_ids text[] NOT NULL, period_start timestamptz NOT NULL, period_end timestamptz NOT NULL, bucket text NOT NULL,
 PRIMARY KEY(workspace,environment,subscription,cycle),
 FOREIGN KEY(workspace,environment,subscription) REFERENCES sr_subscriptions(workspace,environment,id),
 FOREIGN KEY(workspace,environment,bucket) REFERENCES sr_buckets(workspace,environment,id)
);
CREATE TRIGGER sr_entitlement_immutable BEFORE UPDATE OR DELETE ON sr_entitlements FOR EACH ROW EXECUTE FUNCTION sr_immutable();
ALTER TABLE sr_buckets ADD COLUMN source_kind text NOT NULL DEFAULT 'purchased' CHECK(source_kind IN ('subscription','promotion','purchased'));
ALTER TABLE sr_buckets ADD COLUMN service_ids text[];
CREATE INDEX sr_entitlement_customer_period ON sr_entitlements(workspace,environment,customer,period_end);
CREATE INDEX sr_subscriptions_due ON sr_subscriptions(status,current_period_end);
CREATE INDEX sr_audit_tenant ON sr_audit(workspace,environment,created_at,id);
