-- Operational telemetry contains counts only, never request content or personal data.
CREATE TABLE sr_slo_buckets (
 workspace text NOT NULL, environment text NOT NULL, minute timestamptz NOT NULL,
 success bigint NOT NULL DEFAULT 0 CHECK(success>=0), server_error bigint NOT NULL DEFAULT 0 CHECK(server_error>=0),
 upstream_error bigint NOT NULL DEFAULT 0 CHECK(upstream_error>=0),
 PRIMARY KEY(workspace,environment,minute)
);
CREATE INDEX sr_slo_buckets_time ON sr_slo_buckets(minute);
CREATE TABLE sr_slo_alerts (
 workspace text NOT NULL, environment text NOT NULL, high boolean NOT NULL DEFAULT false,
 PRIMARY KEY(workspace,environment)
);
ALTER TABLE sr_users ADD COLUMN deleted_at timestamptz;
ALTER TABLE sr_customers ADD COLUMN deleted_at timestamptz;
-- A stale sign-in or credential enrollment must not recreate PII after deletion.
-- SHARE conflicts with the deletion's user lock and lasts until the writer commits.
CREATE FUNCTION sr_live_user_guard() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE removed timestamptz;
BEGIN
 IF NEW.user_id IS NOT NULL THEN
  SELECT deleted_at INTO removed FROM sr_users WHERE id=NEW.user_id FOR SHARE;
  IF removed IS NOT NULL THEN RAISE EXCEPTION 'account deleted' USING ERRCODE='23514'; END IF;
 END IF;
 RETURN NEW;
END $$;
DO $$ DECLARE tbl text; BEGIN
 FOREACH tbl IN ARRAY ARRAY['sr_user_identities','sr_sessions','sr_user_tokens','sr_oidc_states',
   'sr_mfa_challenges','sr_passkeys','sr_webauthn_challenges','sr_oauth_codes','sr_oauth_grants','sr_personal_keys']
 LOOP
  EXECUTE format('CREATE TRIGGER sr_live_user BEFORE INSERT OR UPDATE OF user_id ON %I FOR EACH ROW EXECUTE FUNCTION sr_live_user_guard()',tbl);
 END LOOP;
END $$;
CREATE FUNCTION sr_user_tombstone_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF OLD.deleted_at IS NOT NULL AND (
  NEW.deleted_at IS NULL OR NEW.status<>'disabled' OR NEW.email IS NOT NULL OR NEW.display_name IS NOT NULL
  OR NEW.password_hash IS NOT NULL OR NEW.totp_secret IS NOT NULL OR NEW.totp_pending IS NOT NULL
  OR NEW.recovery_codes IS NOT NULL OR NEW.platform_role IS NOT NULL
 ) THEN RAISE EXCEPTION 'account deleted' USING ERRCODE='23514'; END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER sr_user_tombstone BEFORE UPDATE ON sr_users FOR EACH ROW EXECUTE FUNCTION sr_user_tombstone_guard();
-- Financial document amounts and identifiers remain immutable. The sole extra
-- transition removes the entire buyer profile during an audited privacy transaction.
CREATE OR REPLACE FUNCTION sr_invoice_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF TG_OP='DELETE' THEN RAISE EXCEPTION 'append-only record'; END IF;
 IF current_setting('superrouter.privacy',true) = 'on'
    AND NEW.buyer = '{}'::jsonb
    AND (to_jsonb(NEW)-'buyer') = (to_jsonb(OLD)-'buyer') THEN RETURN NEW; END IF;
 IF (to_jsonb(NEW) - ARRAY['status','paid_at','voided_at','reason']) <> (to_jsonb(OLD) - ARRAY['status','paid_at','voided_at','reason'])
    OR OLD.status <> 'open' OR NEW.status NOT IN ('paid','void') THEN
  RAISE EXCEPTION 'invoice is immutable';
 END IF;
 RETURN NEW;
END $$;
