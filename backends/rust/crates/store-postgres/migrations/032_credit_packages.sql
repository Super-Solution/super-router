-- One-time credit packages a workspace sells to its customers (wallet top-ups).
CREATE TABLE sr_credit_packages (
 workspace text NOT NULL, environment text NOT NULL, id text NOT NULL, config jsonb NOT NULL,
 updated_at timestamptz NOT NULL DEFAULT now(), PRIMARY KEY(workspace,environment,id)
);
-- A package order belongs to a customer, not a subscription; the package is snapshotted in `plan`.
ALTER TABLE sr_payment_orders ALTER COLUMN subscription DROP NOT NULL;
ALTER TABLE sr_payment_orders ADD COLUMN customer text;
ALTER TABLE sr_payment_orders DROP CONSTRAINT sr_payment_orders_kind_check;
ALTER TABLE sr_payment_orders ADD CONSTRAINT sr_payment_orders_kind_check CHECK(kind IN ('cycle','proration','package'));
ALTER TABLE sr_payment_orders ADD CONSTRAINT sr_payment_orders_owner CHECK(
 (kind='package' AND subscription IS NULL AND customer IS NOT NULL) OR (kind<>'package' AND subscription IS NOT NULL AND customer IS NULL));
ALTER TABLE sr_payment_orders ADD CONSTRAINT sr_payment_orders_customer_fkey
 FOREIGN KEY(workspace,environment,customer) REFERENCES sr_customers(workspace,environment,id);
CREATE INDEX sr_payment_orders_customer ON sr_payment_orders(workspace,environment,customer,period_start DESC) WHERE customer IS NOT NULL;
