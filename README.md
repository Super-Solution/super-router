# SuperRouter

**English** · [繁體中文](README.zh-TW.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja.md)

AI routing, billing and serving infrastructure that you embed as a set of packages. Define a **Service** (an AI capability you sell), pick the models behind it, set prices, subscriptions and credits, and let SuperRouter route each request, meter the usage and settle the books.

> **Status: development release (0.1.0).** The local core runs end to end: service versions, model routing, PostgreSQL reserve/settle accounting, HTTP + SSE, an SDK, a recovery worker, multi-tenant roles and keys, subscriptions and scoped credits. The OpenAI and Anthropic adapters are tested against official-format fixtures but have not been accepted against live upstreams, and the three hosted-payment adapters have not been run with real merchant accounts. There is no hosted Cloud and no production-readiness claim.

## What it does

- **Service-centric routing.** A Service binds to a specific model, an explicit pool, or a task-level tier. Hard limits (region, tags, cost ceiling) are applied first, then weighted scoring, with bounded fallback only when an upstream explicitly refuses.
- **Exact billing.** Integer (BigInt) pricing with input, cached, cache-write and output rates. Funds are reserved before the call and settled after it; retries never double-charge a customer, and an unknown upstream outcome keeps the hold for manual reconciliation instead of silently refunding.
- **Idempotent serving.** HTTP/SSE with idempotency keys, replay status, cancel intent and customer-bound principals. Prompts and completions are not stored.
- **Subscriptions and credits.** Plans, fixed-period subscriptions, credit buckets, grants and paywall-style insufficient-credit handling.
- **Hosted payments.** Adapters for Stripe Checkout, Epay V1 and OEN (應援), with signature verification and authoritative lookups.
- **Recovery.** A worker releases expired reservations, re-settles confirmed usage and parks unknown dispatches for reconciliation.

## Packages

Folders group packages by role: `packages/` holds libraries (`adapters/` plug the core into a database, a payment or a model provider; `bindings/<framework>` holds UI bindings), and `backends/<language>/` holds deployable services. Another UI framework or another language is one more folder. npm names do not depend on the folder.

| Path | Purpose |
| --- | --- |
| `packages/contracts` | Runtime schemas, types, errors, SSE parser |
| `packages/core` | Routing, BigInt pricing, reserve / execute / settle flow, mock adapter |
| `packages/adapters/storage-postgres` | Migrations, tenant scoping, accounts / buckets / journal, requests, outbox and recovery |
| `packages/adapters/provider-openai` | OpenAI Responses text-streaming adapter |
| `packages/adapters/provider-anthropic` | Anthropic Messages text-streaming adapter |
| `packages/adapters/payments` | Stripe, Epay V1 and OEN hosted-checkout adapters |
| `packages/sdk` | HTTP client, SSE, replay state and cancellation |
| `backends/typescript/billing-presets` | Server-only preset validation, idempotent publishing, customer BFF |
| `packages/bindings/react` | React components: pricing table, checkout status, credit balance, paywall, usage summary |
| `backends/typescript/server` | HTTP + SSE server: `createRouterServer`, payment integration wiring |
| `backends/typescript/worker` | Recovery worker library and `super-router-worker` binary |
| `backends/go`, `backends/python` | Proofs of concept of the pricing core, tested against `conformance/` (see [backends/README.md](backends/README.md)) |
| `apps/server`, `apps/worker` | Local demo entry points (mock model, demo workspace) |

Packages are published under the `@supersolutions` scope, for example `@supersolutions/super-router-core` and `@supersolutions/super-router-sdk`.

## Quick start

Requires Node.js 24, npm and Docker. Run everything from the repository root.

```sh
npm ci
npm run build
npm run db:up
npm run demo
npm run demo:subscription
npm run demo:payments
```

The demos use a mock model, so they cost nothing. The database, HTTP, streaming and accounting are real. Expect one model call that charges 35 test atoms, and a retry with the same key that charges nothing more. See [examples/minimal-chat](examples/minimal-chat/README.md).

```sh
npm run typecheck
npm test
npm run contracts
```

Tests need the development database. Set `DATABASE_URL` to override the default `localhost:25432`. Tests use random workspaces and do not delete existing data. `npm run db:down` stops the dedicated container and keeps its volume. Copy `.env.example` for server settings.

## Documentation

Detailed design documents are currently written in Traditional Chinese.

- [PRD](docs/PRD.md), [Architecture](docs/ARCHITECTURE.md), [Data model](docs/DATA_MODEL.md), [Target API](docs/API_SPEC.md), [Implemented OpenAPI](docs/generated/openapi.json)
- [Payments](docs/PAYMENTS.md), [P2 billing](docs/P2_BILLING.md), [Paid components and presets](docs/PAID_COMPONENTS_PRESETS.md)
- [P0 decisions](docs/adr/0001-p0-baseline.md), [Provider spike](docs/PROVIDER_SPIKE.md), [Validation record and limits](docs/P1_VALIDATION.md)
- [Examples](examples/README.md), [Agent skill plan](skills/README.md)

## Known limits

No personal-wallet authorization, no full admin UI, no public SaaS, and the Cloud rule that forbids upstream BYOK exists only as a release-policy test. Payments are fixed-period manual renewals rather than automatic recurring charges. An unknown upstream result keeps its hold and goes to manual reconciliation; it is never retried for free.

## License

Apache License 2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE).

Copyright 2026 SUPER SOLUTION LABS INC.

Contact: contact@supersolution.io
