CREATE TABLE sr_platform_credit_packages (
 id text PRIMARY KEY, config jsonb NOT NULL, updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE sr_platform_credit_orders (
 id text PRIMARY KEY, workspace text NOT NULL, environment text NOT NULL,
 package jsonb NOT NULL, integration text NOT NULL, identity text NOT NULL,
 reference text, url text, state text NOT NULL DEFAULT 'creating'
 CHECK(state IN ('creating','ready','unknown','succeeded','disputed')),
 command_key text NOT NULL, created_at timestamptz NOT NULL DEFAULT now(),
 UNIQUE(workspace,environment,command_key), UNIQUE(integration,reference)
);
CREATE TABLE sr_billing_policies (
 workspace text NOT NULL, environment text NOT NULL, config jsonb NOT NULL DEFAULT '{}',
 low_armed boolean NOT NULL DEFAULT true, PRIMARY KEY(workspace,environment)
);
ALTER TABLE sr_customers ADD COLUMN suspended boolean NOT NULL DEFAULT false;
CREATE TABLE sr_payment_disputes (
 integration text NOT NULL, id text NOT NULL, workspace text NOT NULL, environment text NOT NULL,
 order_id text NOT NULL, details jsonb NOT NULL, created_at timestamptz NOT NULL DEFAULT now(),
 PRIMARY KEY(integration,id)
);
CREATE TRIGGER sr_payment_dispute_immutable BEFORE UPDATE OR DELETE ON sr_payment_disputes FOR EACH ROW EXECUTE FUNCTION sr_immutable();
CREATE TABLE sr_receivables (
 workspace text NOT NULL, environment text NOT NULL, id text NOT NULL, customer text NOT NULL,
 asset text NOT NULL, amount numeric(38,0) NOT NULL CHECK(amount>0), reason text NOT NULL,
 invoice_id text, created_at timestamptz NOT NULL DEFAULT now(),
 PRIMARY KEY(workspace,environment,id)
);
