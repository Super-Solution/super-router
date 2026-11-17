-- Uploads may predate a router request; retain opaque privacy owners independently.
ALTER TABLE sr_media_objects ADD COLUMN privacy_subjects text[] NOT NULL DEFAULT '{}';
CREATE INDEX sr_media_privacy_subjects ON sr_media_objects USING gin(privacy_subjects);
UPDATE sr_media_objects m SET privacy_subjects=ARRAY['customer:' || r.customer]
FROM sr_requests r
WHERE (m.workspace,m.environment,m.request)=(r.workspace,r.environment,r.id);
UPDATE sr_media_objects m SET expires_at=LEAST(m.expires_at,now())
WHERE EXISTS (SELECT 1 FROM sr_customers c WHERE c.workspace=m.workspace
  AND c.environment=m.environment AND c.deleted_at IS NOT NULL
  AND ('customer:' || c.id)=ANY(m.privacy_subjects))
OR EXISTS (SELECT 1 FROM sr_tasks t WHERE (t.workspace,t.environment,t.id)=(m.workspace,m.environment,m.request)
  AND t.principal->>'privacy_deleted'='true');
