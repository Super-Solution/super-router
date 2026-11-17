CREATE TABLE sr_checkouts (
 workspace text NOT NULL, environment text NOT NULL, order_id text NOT NULL,
 integration text NOT NULL, identity text NOT NULL,
 state text NOT NULL CHECK(state IN ('creating','ready','unknown')),
 reference text, url text, created_at timestamptz NOT NULL DEFAULT now(),
 PRIMARY KEY(workspace,environment,order_id),
 UNIQUE(integration,reference),
 FOREIGN KEY(workspace,environment,order_id) REFERENCES sr_payment_orders(workspace,environment,id)
);
