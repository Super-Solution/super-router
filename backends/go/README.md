# Super Router, Go

A complete data plane in Go: exact pricing, offering routing, idempotent admission with reserve and settle, a
`net/http` server, providers, and storage you can swap. The TypeScript packages are the reference, and every
Go package reproduces the shared data in [`../../conformance`](../../conformance/README.md).

```
backends/go
├── contracts/        wire types, strict parsing of untrusted JSON, canonical JSON for the idempotency digest
├── pricing/          Charge, Bound, CompileCreditPrice (exact big-integer money)
├── routing/          Route: hard limits, weighted scores, ordering
├── router/           Router.Infer plus the Store and DataStore ports and a mock provider
├── store/
│   ├── memory/       in-memory DataStore (tests, demos, single-process self-hosting)
│   └── postgres/     PostgreSQL DataStore on database/sql, shared schema and migrations
├── provider/
│   ├── openai/       Responses API
│   └── anthropic/    Messages API
├── server/           the HTTP API as an http.Handler
└── cmd/super-router-server   a ready-to-run binary
```

## Run it

```bash
export SUPERROUTER_API_KEY=sr_$(openssl rand -hex 32)
export SUPERROUTER_DIGEST_KEY=$(openssl rand -hex 32)
export SUPERROUTER_MOCK=1                 # or OPENAI_API_KEY / ANTHROPIC_API_KEY
export DATABASE_URL=postgres://...        # optional: unset means the in-memory store
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

`conformance/dataplane.v1.json` holds 28 scenarios (settlement, fallback, replay, expiry, bucket order, cancel,
recovery and more). `dataplanetest.Concurrency` adds the invariants that must hold under contention.

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

Not yet in Go: payments, subscriptions and the control plane (members, keys, plans). The TypeScript server has them.

Apache-2.0. Copyright SUPER SOLUTION LABS INC.
