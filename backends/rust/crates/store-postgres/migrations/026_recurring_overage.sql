CREATE TABLE sr_overage_consents (
 workspace text NOT NULL, environment text NOT NULL, customer text NOT NULL, service text NOT NULL,
 terms_version text NOT NULL, actor text NOT NULL, consent boolean NOT NULL, recorded_at timestamptz NOT NULL DEFAULT now(),
 PRIMARY KEY(workspace,environment,customer,service)
);
CREATE TABLE sr_overage_reservations (
 workspace text NOT NULL, environment text NOT NULL, request text NOT NULL, customer text NOT NULL, service text NOT NULL,
 policy jsonb NOT NULL, credit_held numeric(38,0) NOT NULL, held numeric(38,0) NOT NULL,
 charged numeric(38,0) NOT NULL DEFAULT 0, released boolean NOT NULL DEFAULT false,
 PRIMARY KEY(workspace,environment,request)
);
ALTER TABLE sr_subscriptions ADD COLUMN recurring_integration text;
ALTER TABLE sr_subscriptions ADD COLUMN recurring_reference text;
CREATE UNIQUE INDEX sr_subscription_recurring ON sr_subscriptions(recurring_integration,recurring_reference) WHERE recurring_reference IS NOT NULL;
CREATE TABLE sr_recurring_invoices (
 integration text NOT NULL, invoice text NOT NULL, workspace text NOT NULL, environment text NOT NULL,
 subscription text NOT NULL, period_start timestamptz NOT NULL, period_end timestamptz NOT NULL,
 PRIMARY KEY(integration,invoice), UNIQUE(workspace,environment,subscription,period_start)
);

ALTER TABLE sr_invoices DROP CONSTRAINT sr_invoices_check;
ALTER TABLE sr_invoices ADD CHECK((kind='invoice' AND credit_for IS NULL) OR (kind='credit_note' AND credit_for IS NOT NULL AND order_id IS NULL AND status='issued'));
