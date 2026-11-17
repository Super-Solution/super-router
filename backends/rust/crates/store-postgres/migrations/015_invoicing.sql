-- Invoices, tax settings, customer billing profiles and prorated plan changes.
CREATE TABLE sr_billing_settings (
 workspace text NOT NULL, environment text NOT NULL, config jsonb NOT NULL,
 -- Tax webhook signing secret, sealed with the host secret key.
 tax_secret text, updated_at timestamptz NOT NULL DEFAULT now(),
 PRIMARY KEY(workspace,environment)
);
ALTER TABLE sr_customers ADD COLUMN billing jsonb;
CREATE TABLE sr_invoice_counters (
 workspace text NOT NULL, environment text NOT NULL, prefix text NOT NULL, next bigint NOT NULL,
 PRIMARY KEY(workspace,environment,prefix)
);
-- Orders: a cycle order per period, plus prorated upgrade orders inside a period.
ALTER TABLE sr_payment_orders ADD COLUMN kind text NOT NULL DEFAULT 'cycle' CHECK(kind IN ('cycle','proration'));
ALTER TABLE sr_payment_orders ADD COLUMN proration jsonb;
ALTER TABLE sr_payment_orders DROP CONSTRAINT sr_payment_orders_workspace_environment_subscription_cycle_key;
CREATE UNIQUE INDEX sr_payment_orders_cycle ON sr_payment_orders(workspace,environment,subscription,cycle) WHERE kind='cycle';
CREATE UNIQUE INDEX sr_payment_orders_open_proration ON sr_payment_orders(workspace,environment,subscription,cycle) WHERE kind='proration' AND state='pending';
-- A voided invoice's order can never be paid into credits.
ALTER TABLE sr_payment_orders DROP CONSTRAINT sr_payment_orders_state_check;
ALTER TABLE sr_payment_orders ADD CONSTRAINT sr_payment_orders_state_check CHECK(state IN ('pending','failed','succeeded','review_required','void'));
-- An upgrade adds an entitlement part inside the same cycle.
ALTER TABLE sr_entitlements ADD COLUMN part integer NOT NULL DEFAULT 0 CHECK(part>=0);
ALTER TABLE sr_entitlements DROP CONSTRAINT sr_entitlements_pkey;
ALTER TABLE sr_entitlements ADD PRIMARY KEY(workspace,environment,subscription,cycle,part);
CREATE TABLE sr_invoices (
 workspace text NOT NULL, environment text NOT NULL, id text NOT NULL, number text NOT NULL,
 kind text NOT NULL CHECK(kind IN ('invoice','credit_note')),
 customer text NOT NULL, order_id text, credit_for text,
 currency text NOT NULL CHECK(currency ~ '^[A-Z]{3}$'),
 subtotal_minor numeric(38,0) NOT NULL CHECK(subtotal_minor>=0),
 tax_minor numeric(38,0) NOT NULL CHECK(tax_minor>=0),
 total_minor numeric(38,0) NOT NULL CHECK(total_minor>=0),
 tax_inclusive boolean NOT NULL,
 lines jsonb NOT NULL, tax jsonb NOT NULL, seller jsonb NOT NULL, buyer jsonb NOT NULL,
 status text NOT NULL CHECK(status IN ('open','paid','void','issued')),
 reason text, issued_at timestamptz NOT NULL DEFAULT now(), paid_at timestamptz, voided_at timestamptz,
 PRIMARY KEY(workspace,environment,id), UNIQUE(workspace,environment,number),
 FOREIGN KEY(workspace,environment,customer) REFERENCES sr_customers(workspace,environment,id),
 FOREIGN KEY(workspace,environment,order_id) REFERENCES sr_payment_orders(workspace,environment,id),
 FOREIGN KEY(workspace,environment,credit_for) REFERENCES sr_invoices(workspace,environment,id),
 CHECK((kind='invoice' AND order_id IS NOT NULL AND credit_for IS NULL) OR (kind='credit_note' AND credit_for IS NOT NULL AND order_id IS NULL AND status='issued'))
);
CREATE UNIQUE INDEX sr_invoices_order ON sr_invoices(workspace,environment,order_id) WHERE order_id IS NOT NULL;
CREATE INDEX sr_invoices_customer ON sr_invoices(workspace,environment,customer,issued_at);
-- Issued documents are append-only: only open -> paid / open -> void may change, once.
CREATE FUNCTION sr_invoice_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF TG_OP='DELETE' THEN RAISE EXCEPTION 'append-only record'; END IF;
 IF (to_jsonb(NEW) - ARRAY['status','paid_at','voided_at','reason']) <> (to_jsonb(OLD) - ARRAY['status','paid_at','voided_at','reason'])
    OR OLD.status <> 'open' OR NEW.status NOT IN ('paid','void') THEN
  RAISE EXCEPTION 'invoice is immutable';
 END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER sr_invoice_immutable BEFORE UPDATE OR DELETE ON sr_invoices FOR EACH ROW EXECUTE FUNCTION sr_invoice_guard();
