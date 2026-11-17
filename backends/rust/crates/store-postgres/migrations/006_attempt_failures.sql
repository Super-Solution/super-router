-- Operator-visible reason an attempt did not succeed (adapter code such as upstream_http_429,
-- stream_interrupted, cancelled). Customers only ever see the public failure vocabulary.
ALTER TABLE sr_attempts ADD COLUMN IF NOT EXISTS failure_code text;
