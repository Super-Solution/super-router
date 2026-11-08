# Engineering improvement log

What was found and changed, what is still open, and what the three backends must keep in step. Read this before
starting a new round, and add to it instead of repeating work.

## Gate

`npm run check` (`scripts/check-all.sh [ts|go|python]`) runs everything against one PostgreSQL with database tests
required. Hosted CI is not used. Last full run, 2026-11-07: TypeScript 140 tests, Go `-race` all packages, Python 249 tests
(also on 3.10), ruff, mypy and gofmt clean.

## 2026-11-07

Found and fixed:

- **Usage with missing counters was billed as free (Go and Python providers).** The TypeScript adapters refuse a
  completed response whose usage lacks `input_tokens` or `output_tokens` (`NaN` fails `Meters.parse`). Go decoded a
  missing counter to `0` and Python defaulted it to `0`, so a response with `usage: {}` settled at zero cost. Both now
  refuse it with `invalid_usage` (outcome unknown, funds stay held). Malformed or negative counters, including cache
  counters, are refused the same way, and Anthropic's `message_delta` may still complete counters that
  `message_start` lacked. Regression tests in both languages.
- **Go credit-rates endpoint had no HTTP test.** Added: publish, stale version (409), malformed bodies, scope.
- **Formatting drift** in two TypeScript files, which `format:check` flagged.

Added: the local gate above; Go and Python database-backed keys with write revalidation (interop-tested with keys
issued by the TypeScript control plane); Python data plane and ASGI server; translated READMEs moved to `docs/i18n/`.

## Contracts the backends must keep in step

- `conformance/*.json` is normative. Change behaviour there first, then in every backend.
- Postgres migrations are byte-identical in `packages/adapters/storage-postgres/migrations`,
  `backends/go/store/postgres/migrations` and `backends/python/src/super_router/stores/migrations`; tests enforce it.
- Provider failure classification: definitive `{400,401,403,404,422}`, retryable `429`, everything else unknown;
  redirects are never followed; usage must be complete or the outcome is unknown.
- Money is decimal strings and exact integers. No floats in pricing or settlement.

## Open

- Payments, subscriptions and the control plane (members, keys, plans) exist only in TypeScript. Go and Python can run
  in cloud mode against the TypeScript-managed tables (`SUPERROUTER_AUTH=database`), which is the cheaper path.
- Providers are tested against fake upstreams only, not the live OpenAI and Anthropic APIs.
- No benchmarks for the Go and Python stores yet; the memory stores scan per tenant.
- Python JSON number text differs from Go and TypeScript for integral floats (`1.0` vs `1`) in the dry-run `score`;
  values are equal.

## Rounds of 15 (audit, fix, local gate, push)

### Round 2 (2026-11-08): request bodies in the TypeScript handler

- **Chunked bodies were buffered before the size check.** `rawBody` compared `Content-Length` with the 256 kB limit and
  then called `arrayBuffer()`, so a request with no `Content-Length` (chunked) was read in full first. The payment
  notification route reads its body before any authentication, so an anonymous client could make the process buffer an
  unbounded body. The body is now read chunk by chunk and the stream is cancelled the moment it passes the limit
  (413 `body_too_large`). A test feeds an endless stream: before the fix it never ended, now it answers 413 after reading 256 kB.
- The Go (`http.MaxBytesReader`) and Python (running total in the ASGI receive loop) servers already bounded the read.
- Gate: `npm run typecheck`, `format:check`, all 141 tests against a real PostgreSQL (none skipped).
