ALTER TABLE sr_requests ADD COLUMN IF NOT EXISTS route_decision jsonb;
ALTER TABLE sr_buckets ADD COLUMN IF NOT EXISTS granted numeric(38,0);
UPDATE sr_buckets b SET granted=j.amount FROM sr_journal j WHERE (b.workspace,b.environment,'grant:'||b.source)=(j.workspace,j.environment,j.business_key) AND b.granted IS NULL;
CREATE OR REPLACE FUNCTION sr_service_immutable() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF OLD.config IS DISTINCT FROM NEW.config OR OLD.version<>NEW.version OR OLD.id<>NEW.id OR OLD.workspace<>NEW.workspace OR OLD.environment<>NEW.environment THEN
  RAISE EXCEPTION 'published service is immutable';
 END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER sr_service_immutable BEFORE UPDATE ON sr_services FOR EACH ROW EXECUTE FUNCTION sr_service_immutable();
CREATE TRIGGER sr_settlement_immutable BEFORE UPDATE OR DELETE ON sr_settlements FOR EACH ROW EXECUTE FUNCTION sr_immutable();
CREATE TABLE sr_refunds (
 workspace text NOT NULL, environment text NOT NULL, request text NOT NULL, kind text NOT NULL, business_key text NOT NULL,
 amount numeric(38,0) NOT NULL CHECK(amount>0), reason text NOT NULL, created_at timestamptz NOT NULL DEFAULT now(),
 PRIMARY KEY(workspace,environment,business_key),
 FOREIGN KEY(workspace,environment,request,kind) REFERENCES sr_settlements(workspace,environment,request,kind)
);
CREATE TRIGGER sr_refund_immutable BEFORE UPDATE OR DELETE ON sr_refunds FOR EACH ROW EXECUTE FUNCTION sr_immutable();
ALTER TABLE sr_allocations ADD COLUMN IF NOT EXISTS charged numeric(38,0) NOT NULL DEFAULT 0;
ALTER TABLE sr_allocations ADD COLUMN IF NOT EXISTS refunded numeric(38,0) NOT NULL DEFAULT 0;
ALTER TABLE sr_allocations ADD CONSTRAINT sr_allocation_charges CHECK(charged>=0 AND refunded>=0 AND refunded<=charged AND charged<=amount);
