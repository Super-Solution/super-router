-- Keys inserted by a store that predates key lineages (the open Go/Python data planes, or older
-- tooling) omit budget_id and budget_anchor. Fill them the way 007 backfilled existing rows:
-- a key is its own lineage and its window anchors at issue time.
CREATE OR REPLACE FUNCTION sr_keys_lineage_defaults() RETURNS trigger AS $$
BEGIN
  NEW.budget_id := COALESCE(NEW.budget_id, NEW.id);
  NEW.budget_anchor := COALESCE(NEW.budget_anchor, NEW.created_at, now());
  RETURN NEW;
END $$ LANGUAGE plpgsql;
DROP TRIGGER IF EXISTS sr_keys_lineage_defaults ON sr_keys;
CREATE TRIGGER sr_keys_lineage_defaults BEFORE INSERT ON sr_keys
  FOR EACH ROW EXECUTE FUNCTION sr_keys_lineage_defaults();
