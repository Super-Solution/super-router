# Super Router, Python

A complete data plane in Python: exact pricing, offering routing, idempotent admission with reserve and settle,
an ASGI server, providers, and storage you can swap. The TypeScript packages are the reference, and every module
here reproduces the shared data in [`../../conformance`](../../conformance/README.md).

```
src/super_router
├── pricing.py        charge, bound, compile_credit_price (exact integer money)
├── routing.py        route: hard limits, weighted scores, ordering
├── contracts.py      strict parsing of untrusted JSON, Principal, ProviderError, provider events
├── router.py         Router.infer plus the Store and DataStore ports and a mock provider
├── stores/
│   ├── memory.py     in-memory DataStore (tests, demos, single-process self-hosting)
│   ├── postgres.py   PostgreSQL DataStore, shared schema and migrations
│   └── sql.py        the small Database port, and its psycopg 3 adapter
├── providers/        OpenAI Responses and Anthropic Messages on httpx
├── server.py         the HTTP API as an ASGI app, no dependencies
└── __main__.py       super-router-server
```

`pip install super-router` has no dependencies and gives you pricing, routing, the router and the memory store.
Extras: `[postgres]` (psycopg 3), `[http]` (httpx, for providers), `[server]` (both plus uvicorn).

## Run it

```bash
pip install "super-router[server]"
export SUPERROUTER_API_KEY=sr_$(openssl rand -hex 32)
export SUPERROUTER_DIGEST_KEY=$(openssl rand -hex 32)
export SUPERROUTER_MOCK=1                 # or OPENAI_API_KEY / ANTHROPIC_API_KEY
export DATABASE_URL=postgres://...        # optional: unset means the in-memory store
super-router-server                       # or: python -m super_router
```

The environment variables are the same as the Go binary's (`ADDR`, `SUPERROUTER_WORKSPACE`,
`SUPERROUTER_ENVIRONMENT`, `SUPERROUTER_CUSTOMER`; the module docstring of `super_router.__main__` lists them all).

## Embed it

`create_app` returns an ASGI 3 application, so it runs on uvicorn, hypercorn and granian, and mounts inside
Starlette, FastAPI and Django's ASGI router:

```python
from super_router.providers import OpenAIProvider
from super_router.router import Router
from super_router.server import Key, StaticKeys, create_app
from super_router.stores.postgres import PostgresStore
from super_router.stores.sql import PsycopgDatabase

db = await PsycopgDatabase.connect(DATABASE_URL)
store = PostgresStore(db)
await store.migrate()
router = Router(store, {"openai": OpenAIProvider(OPENAI_API_KEY)}, DIGEST_KEY)
api = create_app(router, store, StaticKeys([Key(secret, principal)]))

app.mount("/router", api)  # FastAPI / Starlette
```

Authentication is the `Authenticator` port; to back keys with your database implement
`async def authenticate(self, token: str) -> Principal` and raise `RouterError("invalid_key", 401)` for an unknown key.
Call `await store.recover()` on a timer (the binary does it every 30 seconds) to settle or release requests whose
deadline passed, for example after a crash mid-call.

Providers are anything with `invoke(invocation, offering) -> AsyncIterator[Delta | Usage | Done]`. Raise
`ProviderError(definitive, retryable, code)`: definitive means the provider certainly did no work, so the router may
release the reservation and try another offering; anything else keeps the funds held until the outcome is known.

## Bring your own database

Storage is the `DataStore` protocol (`Store` plus publish, list, balance, grant, disable and recover). The
PostgreSQL store runs on a thirty-line `Database` port (`transaction()` yielding `execute`, `fetch_one`, `fetch_all`
with `$1`-style placeholders), so another driver or an ORM's pool only needs a small adapter: asyncpg already speaks
`$n`; SQLAlchemy can run the same text through `exec_driver_sql` after rewriting `$n` to `:pn`. To support another
database, implement the protocol and run the shared scenarios against it (see `tests/test_dataplane.py` and
`tests/dataplane_runner.py`). `conformance/dataplane.v1.json` holds 28 scenarios (settlement, fallback, replay,
expiry, bucket order, cancel, recovery and more).

## Test

```bash
pip install -e ".[dev]"
pytest                                    # postgres tests skip when no database answers
SR_REQUIRE_POSTGRES=1 DATABASE_URL=postgres://... pytest    # fail instead of skip (CI)
ruff check . && ruff format --check . && mypy
```

The Postgres tests use `postgresql://superrouter:local-development-only@127.0.0.1:25432/superrouter` unless
`DATABASE_URL` is set (the repo's `docker compose` database). Python 3.10 and newer.

Not yet in Python: payments, subscriptions and the control plane (members, keys, plans). The TypeScript server has them.

Apache-2.0. Copyright SUPER SOLUTION LABS INC.
