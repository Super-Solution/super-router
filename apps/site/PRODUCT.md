# Product

<!-- impeccable:product-schema 1 -->

## Platform

web

## Stack

Astro (user choice), in `apps/site` of the SuperRouter monorepo. Static output; bilingual (Traditional Chinese and English, switchable).

## Business Model

The Next.js / Vercel model (user confirmed, 2026-11-07):

- **Open-source base (like Next.js):** the SuperRouter packages and server are published (npm). Anyone can self-host the full routing, billing, ledger, plans, invoices and tax stack.
- **SuperRouter Cloud (like Vercel):** the team operates the official hosted SuperRouter. It is the default destination: the open-source SDK and clients point at Cloud unless a developer sets their own base URL.
- **Cloud-only features:** the reason to choose Cloud over self-hosting:
  1. **Shared wallet across apps:** an end user buys credits once on Cloud, then spends them in any app that offers "Sign in with SuperRouter" (like Sign in with ChatGPT).
  2. **Keys only:** developers apply for a key and build; no server, upstream contracts or payment integration needed.
  3. **Per-key statistics:** usage, spend and users for each key / app.
  4. **Developer markup:** a developer can price credits above Cloud's base price and earn the difference from their users' token usage.
- **How Cloud makes money (tentative):** selling subscriptions and credits to users, plus the platform's share of usage. Plan names, prices and what a subscription includes are undecided.

Which code is open versus Cloud-only (repo split, license) is undecided.

## Users

Teams building AI products who want to sell AI features to their own customers. Two readers arrive at the site and get their own path:

- **Customer (the team deciding):** founders, product and business owners. Their job is to judge whether SuperRouter lets them charge for AI safely: pricing their own plans, credits, invoices and tax, not losing money on failed or unclear upstream calls, and keeping data and keys on their own infrastructure.
- **Dev (the engineers adopting it):** backend engineers. Their job is to judge integration cost and correctness: OpenAI-compatible API (change the base URL), SDK on npm, self-host with Docker Compose and PostgreSQL, routing and fallback behaviour, how billing settles.

## Product Purpose

SuperRouter is a self-hosted AI gateway with a billing ledger built in. It routes requests to AI providers and records, for every request, what it cost upstream, what the customer was charged and how it settled. Success: a team ships paid AI features without building routing, metering, credits, subscriptions, invoices and reconciliation themselves, and without leaking money on failures.

## Positioning

Routing plus billing that reconciles. Neighbouring gateways forward requests; SuperRouter reserves before calling, settles on actual usage, never treats an unclear outcome as free or retries it blindly (it goes to a reconciliation queue), and keeps three ledgers apart (provider cost, platform sale, customer sale). Lineage: rewrite of the team's Go gateway clawRouter.

## Operating Context

- Evaluated by reading docs, the OpenAPI spec and running `docker compose up` with PostgreSQL.
- Integrated through `/v1/chat/completions` (OpenAI SDK, new base URL), the native API, or `@superrouter/sdk`.
- Operated through the bundled admin console (services, keys, customers, credits, plans, invoices, tax, health, webhooks, audit).

## Capabilities and Constraints

Confirmed and implemented (see repo README and docs):

- Upstreams: OpenAI, Anthropic, any OpenAI-compatible Chat Completions server (vLLM, Ollama, OpenRouter, DeepSeek…); model discovery.
- Routing: versioned Services, specific / pool / tier selection, hard limits before scoring, shared health and circuit breakers, fallback only when the upstream definitely did not execute, tool calling.
- Billing: BigInt atoms, reserve then settle, idempotent replays, reconciliation queue, credit buckets with expiry and Service scope, subscriptions, prorated upgrades, segment prices, key spend/rate/concurrency/IP limits.
- Invoicing: invoice per payment order, gapless numbering, immutable records, void, credit notes, tax by none / rate table / developer's tax webhook, invoice events for e-invoice integration.
- Payments: Stripe, Epay V1, OEN hosted checkout (sandbox-verified).
- Tenancy and identity: workspaces, projects, roles; password, OIDC, passkeys, TOTP.
- Ops: webhooks with SSRF protection, metrics, audit, leader worker, probes, Docker image and compose.

Undecided / not yet true: packages are currently private (UNLICENSED); public npm release is planned (user confirmed) but not done; no hosted cloud signup; no public pricing. License after release is undecided.

## Brand Commitments

Name: SuperRouter. No logo or brand assets exist yet. Voice of existing docs: plain, precise, states what is not done.

## Evidence on Hand

- Real product facts and numbers from the repo (tests: 155 passing; docs in `docs/`).
- No customers, testimonials, logos, benchmarks, press or pricing exist. Do not fabricate any.
- Real-provider and live-payment verification is still pending; do not claim production usage.

Built for Cloud (docs/WALLET.md): the shared wallet and "Sign in with SuperRouter" (OAuth + PKCE, spending caps, console wallet, consent and app pages); developer markup tiers (0/10/20/30/50%, invisible to users) with earnings in the ledger; per-app and per-key statistics; Stripe Connect payouts (verified with a fake Stripe only; live verification needs a Connect account); SDK default base URL pointing at Cloud (`CLOUD_BASE_URL` is an `.invalid` placeholder until the Cloud domain is chosen).

## Product Principles

1. Money is never guessed: every unclear outcome is recorded and reconciled, not assumed.
2. The developer keeps control: self-hosted, own keys, own data, own tax logic.
3. Drop-in first: OpenAI-compatible so adoption starts with a base URL change.
4. State limits plainly; never claim what is not built or verified.
