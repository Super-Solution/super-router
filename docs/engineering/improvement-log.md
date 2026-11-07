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
- `docs/` links to `P1_VALIDATION.md` and `P2_BILLING.md` are in the translated READMEs only; keep them in step if
  those files change.
- Python JSON number text differs from Go and TypeScript for integral floats (`1.0` vs `1`) in the dry-run `score`;
  values are equal.
