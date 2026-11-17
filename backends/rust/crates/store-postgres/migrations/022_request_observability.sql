ALTER TABLE sr_requests ADD COLUMN first_token_ms double precision CHECK(first_token_ms>=0);
ALTER TABLE sr_requests ADD COLUMN total_latency_ms double precision CHECK(total_latency_ms>=0);
CREATE INDEX sr_request_page ON sr_requests(workspace,environment,created_at DESC,id DESC);
CREATE INDEX sr_request_service ON sr_requests(workspace,environment,(service->>'id'),created_at DESC);
CREATE INDEX sr_attempts_request ON sr_attempts(workspace,environment,request,created_at DESC,id DESC);
