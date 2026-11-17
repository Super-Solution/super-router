-- Synthetic recovery evidence only: no tenant or personal data.
CREATE TABLE sr_recovery_markers (
 recorded_at timestamptz PRIMARY KEY DEFAULT clock_timestamp(),
 node_id text NOT NULL CHECK(length(node_id) BETWEEN 1 AND 128),
 generation integer NOT NULL CHECK(generation >= 0)
);
-- Inserts are immutable; the leader may delete expired evidence after 48 hours.
CREATE TRIGGER sr_recovery_marker_immutable BEFORE UPDATE ON sr_recovery_markers
 FOR EACH ROW EXECUTE FUNCTION sr_immutable();
CREATE FUNCTION sr_recovery_marker_retention() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF OLD.recorded_at >= clock_timestamp() - interval '48 hours' THEN
  RAISE EXCEPTION 'recovery marker is within retention';
 END IF;
 RETURN OLD;
END $$;
CREATE TRIGGER sr_recovery_marker_retention BEFORE DELETE ON sr_recovery_markers
 FOR EACH ROW EXECUTE FUNCTION sr_recovery_marker_retention();
