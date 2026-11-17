-- Read indexes for the console. IF NOT EXISTS lets operators of large databases build them
-- beforehand with CREATE INDEX CONCURRENTLY (see docs/UPGRADING.md); this is then a no-op.
-- Wallet activity reads a customer's journal entries by account, newest first.
CREATE INDEX IF NOT EXISTS sr_journal_debit_time ON sr_journal(workspace,environment,debit,created_at DESC);
CREATE INDEX IF NOT EXISTS sr_journal_credit_time ON sr_journal(workspace,environment,credit,created_at DESC);
-- Console search and customer lists look up a customer's recent requests.
CREATE INDEX IF NOT EXISTS sr_requests_customer_time ON sr_requests(workspace,environment,customer,created_at DESC);
