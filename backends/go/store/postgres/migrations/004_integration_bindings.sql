CREATE TABLE sr_integration_bindings (
 id text PRIMARY KEY, workspace text NOT NULL, environment text NOT NULL
);
CREATE TRIGGER sr_integration_binding_immutable BEFORE UPDATE OR DELETE ON sr_integration_bindings FOR EACH ROW EXECUTE FUNCTION sr_immutable();
