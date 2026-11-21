# Super Router, Rust

The data plane in Rust: exact big-integer pricing, offering routing, idempotent admission with reserve and settle,
an HTTP API on axum, providers, and storage you can swap. The TypeScript packages are the reference and every crate
reproduces the data in [`conformance`](conformance), the same checks the Go and Python backends run on their copies of the shared data.

> **Its own schema and conformance data.** This backend carries its own PostgreSQL migrations
> ([`crates/store-postgres/migrations`](crates/store-postgres/migrations)) and its own copy of the shared data
> ([`conformance`](conformance)). It supports custom billing units, durable tasks, attempt failures, database-backed
> keys and session/weekly usage windows, which the TypeScript, Go and Python backends in this repository do not have
> yet. Use a database of its own: the other backends' migration history is different. Ten crates are prepared for
> crates.io (`super-router-*`, released together; see [`docs/CRATES-PUBLISHING.md`](../../docs/CRATES-PUBLISHING.md));
> the first release has not been made yet, so until then depend on them as a git dependency pinned to one commit.
> `super-router-cli` and `super-router-dataplane-test` are not published.

```
backends/rust
├── crates/pricing/            Charge, Bound, CompileCreditPrice (exact BigInt money)
├── crates/contracts/          wire types, strict parsing of untrusted JSON, canonical JSON for the idempotency digest
├── crates/routing/            route: hard limits, weighted scores, ordering (bit-exact IEEE scores)
├── crates/router/             Router::infer, the Store / DataStore / Authenticator ports, mock providers
├── crates/store-memory/       in-memory DataStore (tests, demos, single-process self-hosting)
├── crates/store-postgres/     PostgreSQL DataStore on sqlx, shared migrations
├── crates/sse/                bounded server-sent-events reader
├── crates/provider-openai/    Responses API
├── crates/provider-anthropic/ Messages API
├── crates/server/             the HTTP API as an axum Router
├── crates/cli/                the `super-router-server` binary
└── crates/dataplane-test/     runs conformance/dataplane.v1.json against any DataStore, and tools.v1.json against providers
```

## Design notes

- **Providers are streams.** `Provider::invoke` returns a `Stream` of `ProviderEvent`s. Dropping the stream abandons
  the upstream call, which is how cancellation, timeouts and a vanished client stop a request. The router decides what
  a stop means: output already shown, or a lost outcome, keeps the hold for reconciliation and is never refunded.
- **Settlement outlives the caller.** `Router::infer` takes a `CancellationToken` that aborts the provider call only.
  Every storage call after funds are reserved runs to completion; the HTTP layer runs `infer` on its own task so a
  dropped connection cannot skip it.
- **Tool calling matches the reference.** Tools, tool choice, tool calls, tool results, image parts and several system
  messages parse and canonicalise as in TypeScript (`conformance/tools.v1.json`). `Provider::capabilities` says
  whether a provider carries tools and images; an invocation nobody can carry is refused with
  `422 unsupported_capability` before anything is reserved. A started tool call is output: a later failure is partial
  and never falls back. `MockToolProvider` (`mock-tools` under `SUPERROUTER_MOCK=1`) scripts a tool call for demos.
  Inference bodies may be up to 4 MB (`MAX_INFERENCE_BODY_BYTES`), other routes 256 KB.
- **Time is milliseconds since the epoch** (`i64`) at every port, so no date library is needed in the core.
- **Money is `BigInt` and decimal strings**, never floats. Scores are `f64` and match the reference bit for bit.
- Internal failures (`StoreError::Internal`) never reach a customer; they surface as `internal_error` or, once funds
  are reserved, `execution_unknown` with the hold kept.

## Test

```bash
cargo test --workspace                       # PostgreSQL tests skip when no database answers
SR_REQUIRE_POSTGRES=1 cargo test --workspace # fail instead of skip (use this in the gate)
cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check
scripts/check-all.sh rust                    # the same, from the repository root (set DATABASE_URL to its own database)
```

The PostgreSQL tests use `postgresql://superrouter:local-development-only@127.0.0.1:25432/superrouter` unless
`DATABASE_URL` is set (the repository's `docker compose` database).
