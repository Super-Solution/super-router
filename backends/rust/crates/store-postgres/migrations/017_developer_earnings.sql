-- Cloud: developers mark up the wallet price for their app's users and earn the difference.
-- Markup is one of fixed tiers per app; users only ever see the credits they spend.
ALTER TABLE sr_oauth_clients ADD COLUMN markup_bps integer NOT NULL DEFAULT 0 CHECK(markup_bps IN (0,1000,2000,3000,5000));
ALTER TABLE sr_keys ADD COLUMN markup_bps integer CHECK(markup_bps BETWEEN 0 AND 10000);
ALTER TABLE sr_keys ADD COLUMN beneficiary text;
-- Earned (usage) and reversed (refund) amounts in the wallet's credit asset. Append-only;
-- payouts reference the rows they paid.
CREATE TABLE sr_developer_earnings (
 id text PRIMARY KEY, workspace text NOT NULL, environment text NOT NULL,
 request text NOT NULL, client_id text NOT NULL REFERENCES sr_oauth_clients(id),
 kind text NOT NULL CHECK(kind IN ('usage','refund')), business_key text NOT NULL,
 asset text NOT NULL, amount numeric(38,0) NOT NULL,
 created_at timestamptz NOT NULL DEFAULT now(), payout_id text,
 UNIQUE(workspace,environment,business_key),
 CHECK((kind='usage' AND amount>0) OR (kind='refund' AND amount<0))
);
CREATE INDEX sr_developer_earnings_client ON sr_developer_earnings(client_id, created_at);
CREATE INDEX sr_developer_earnings_unpaid ON sr_developer_earnings(client_id) WHERE payout_id IS NULL;
-- Only payout_id may change, once, from null.
CREATE FUNCTION sr_earning_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF TG_OP='DELETE' THEN RAISE EXCEPTION 'append-only record'; END IF;
 IF (to_jsonb(NEW) - 'payout_id') <> (to_jsonb(OLD) - 'payout_id') OR OLD.payout_id IS NOT NULL THEN
  RAISE EXCEPTION 'earning is immutable';
 END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER sr_earning_immutable BEFORE UPDATE OR DELETE ON sr_developer_earnings FOR EACH ROW EXECUTE FUNCTION sr_earning_guard();
