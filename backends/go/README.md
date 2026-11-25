# Super Router, Go

A complete data plane in Go: exact pricing, offering routing, idempotent admission with reserve and settle, a
`net/http` server, providers, and storage you can swap. The TypeScript packages are the reference, and every
Go package reproduces the shared data in [`../../conformance`](../../conformance/README.md). Pricing, contracts,
routing and the data plane scenarios are read from the Rust copy in [`../rust/conformance`](../rust/conformance), which
holds every shared case plus those for the features below that the TypeScript reference does not have yet.

```
backends/go
├── contracts/        wire types, strict parsing of untrusted JSON, canonical JSON for the idempotency digest
├── pricing/          Charge, Bound, CompileCreditPrice (exact big-integer money)
├── routing/          Route: hard limits, weighted scores, ordering
├── router/           Router.Infer plus the Store and DataStore ports and mock providers (text, tools)
├── store/
│   ├── memory/       in-memory DataStore (tests, demos, single-process self-hosting)
│   └── postgres/     PostgreSQL DataStore on database/sql, shared schema and migrations
├── provider/
│   ├── openai/       Responses API
│   ├── anthropic/    Messages API
│   └── gemini/       generateContent (Gemini API, Vertex AI)
├── server/           the HTTP API as an http.Handler
└── cmd/super-router-server   a ready-to-run binary
```

## Run it

```bash
export SUPERROUTER_API_KEY=sr_$(openssl rand -hex 32)
export SUPERROUTER_DIGEST_KEY=$(openssl rand -hex 32)
export SUPERROUTER_MOCK=1                 # or OPENAI_API_KEY / ANTHROPIC_API_KEY / GEMINI_API_KEY
export DATABASE_URL=postgres://...        # optional: unset means the in-memory store
export SUPERROUTER_HEALTH=on              # optional: skip offerings whose own failure repeats
export OPENAI_BASE_URL=https://llm.internal/openai   # optional: another host, without /v1
go run ./cmd/super-router-server
```

## Embed it

Everything is a library. The handler is a plain `http.Handler`, so it mounts on the standard mux and on every
framework that takes one:

```go
rt, _ := router.New(store, map[string]contracts.Provider{"openai": openai.New(key)}, digestKey)
api, _ := server.Handler(server.Options{Router: rt, Store: store, Auth: server.StaticKeys(keys)})

http.Handle("/", api)                   // net/http
r.Mount("/router", http.StripPrefix("/router", api))   // chi
g.Any("/router/*any", gin.WrapH(http.StripPrefix("/router", api)))
e.Any("/router/*", echo.WrapHandler(http.StripPrefix("/router", api)))
```

Authentication is the `server.Authenticator` port; to back keys with your database implement
`Authenticate(ctx, token) (contracts.Principal, error)`.

## Bring your own database

Storage is the `router.DataStore` interface (`router.Store` plus publish, list, balance, grant, disable and
recover). `store/postgres` needs only `*sql.DB`, so any driver works: pgx (`github.com/jackc/pgx/v5/stdlib`),
lib/pq, or the pool your ORM already owns (`gorm.DB.DB()`, `ent`, `bun`, `sqlx`). To support another database,
implement the interface and run the shared scenarios against it:

```go
dataplanetest.Run(t, "mydb", func(t *testing.T) *dataplanetest.Harness { ... })
```

`rust/conformance/dataplane.v1.json` holds 32 scenarios (the 28 shared ones: settlement, fallback, replay, expiry,
bucket order, cancel, recovery and more; and 4 for downgrade and label evidence). `dataplanetest.Concurrency` adds the
invariants that must hold under contention, and `CustomUnits`, `Downgrade` and `StructuredOutput` the features below.

## Test

```bash
go test ./...                                   # postgres tests skip when no database answers
SR_REQUIRE_POSTGRES=1 DATABASE_URL=postgres://... go test ./...   # fail instead of skip (CI)
go test -race ./...
go test ./routing -bench . -benchmem
go test ./contracts -fuzz FuzzParse -fuzztime 30s
go vet ./... && gofmt -l .
```

The Postgres tests use `postgresql://superrouter:local-development-only@127.0.0.1:25432/superrouter` unless
`DATABASE_URL` is set (the repo's `docker compose` database).

## Tool calling

The Go data plane speaks the same tool-calling wire as the TypeScript reference: `input.tools`, `input.tool_choice`,
assistant `tool_calls`, `role: "tool"` results, image parts and several system messages; the stream adds
`output.tool_call.delta` and `output.tool_call.completed`, and the terminal event carries `finish_reason` and `usage`. A call may carry an opaque `signature` (Gemini 3's `thoughtSignature`): it is on the first delta and the completed event, in the result's `tool_calls`, and goes back unchanged when the call is replayed.
A provider says what it can carry through `contracts.CapableProvider`; an invocation that needs tools or images and
finds no capable offering is refused with `422 unsupported_capability` before anything is reserved. Once a call has
started streaming it is output, so a later failure is partial and never falls back. `/v1/inferences` and the routing
dry run accept bodies up to 4 MB (read after authentication); other routes keep 256 KB. `conformance/tools.v1.json`
pins the parsing, the canonical JSON and the OpenAI and Anthropic mappings, and `SUPERROUTER_MOCK=1` also registers a
scripted `mock-tools` provider for demos.

## Beyond the TypeScript reference

These match the Rust data plane and are pinned by its conformance copy:

- **Custom billing units.** A price may bill named units (`"units": {"pages": {"atoms": "7", "per": 3,
  "display_unit": "頁"}}`), a task names its meter (`"meter": "pages"`), and limits put them under
  `"max_usage": {"units": {...}}`. Unit names match `^[a-z][a-z0-9_]{0,63}$` and may not reuse a token or media name.
  A credit-rate version keeps a unit-billed service in units.
- **Rate card.** `pricing.RateCard` prices the kinds of token the wire `Price` has no field for (one-hour cache writes,
  reasoning tokens) and a fixed fee per call, from one value with the usual fallbacks: `Effective`, `Charge` (exact
  sum, rounded up once, plus the fee) and `Bound` (what to hold before the call). No wire format, contract or store
  changes; its cases are `rate_card_charge` and `rate_card_bound` in the Rust conformance copy. `Defaulted` and
  `DefaultedIn` list the kinds priced by fallback (a cache read billed at the full input rate is often a forgotten
  rate, not a decision). `pricing.RateSchedule` picks the card in effect at an instant (`CardAt`, `NextChangeAfter`) and
  `BoundBetween` holds enough for a call that may settle after a price change. `pricing.RateTiers` is long-context
  pricing: the whole call is priced by the card of the tier its prompt reaches (`CardFor`, `Charge`), and `Bound` holds
  the dearest tier a prompt of up to `maxInput` tokens can reach (cases `rate_tiers_charge`, `rate_tiers_bound`).
- **Structured output.** `response_format` (`json_schema` with a name, a schema object and optional description and
  `strict`) excludes offerings without the `structured_output` capability as `structured_output_unsupported`. No
  provider whose `Capabilities()` says `StructuredOutput` receives it (the Gemini provider does and writes
  `responseJsonSchema`); a request with no such candidate is refused with `422 unsupported_capability` before anything
  is reserved.
- **Failure classes and the health book.** `contracts.ClassifyFailure` reads an upstream refusal's status and message and
  returns a `FailureClass`; a failure that belongs to the offering (credentials, model, quota, region, rate limit) is
  definitive and retryable, so the router tries the next offering. `routing.HealthBook` (with `router.WithHealth`)
  skips offerings whose own failure is repeating, with class-based cooldowns and one half-open probe, and fails open
  when every candidate is cooling. Off by default. Cases: `failure.v1.json` (shared by every backend, the three providers read their refusals through it) and `health.v1.json` (Rust and Go only).
- **Finish reasons.** A provider that ends an answer by content policy emits completion `filtered`: stored as partial,
  reported as `finish_reason: content_filter`.
- **Downgrade policy.** A tier service may name a lower tier and its offerings. They pass the same hard limits, are
  tried after every primary offering, emit `routing.downgrade` when tried, and the result reports `downgrade`
  (`from_tier`, `to_tier`) and the lower tier.
- **Label evidence and quality metadata.** An offering may carry `label_evidence` per label (source, `verified` or
  `unverified`, `expires_at`) and `quality_metadata`. Expired evidence excludes the offering as `label_expired`; with
  `require_verified_labels`, missing or unverified evidence excludes it as `label_unverified`. Both are kept in the
  request's route decision.
- **Offering capabilities.** `capabilities` lists `tools` and `structured_output`.

Not yet in Go: payments, subscriptions and the control plane (members, keys, plans). The TypeScript server has them.

Apache-2.0. Copyright SUPER SOLUTION LABS INC.
