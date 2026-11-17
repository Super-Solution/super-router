CREATE TABLE sr_media_objects (
  key text PRIMARY KEY,
  workspace text NOT NULL,
  environment text NOT NULL,
  request text NOT NULL,
  content_type text NOT NULL,
  expires_at timestamptz NOT NULL
);
CREATE INDEX sr_media_expiry ON sr_media_objects(expires_at);
