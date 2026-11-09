# Superchat with SuperRouter billing

A small Node monorepo you run on your own machine. It puts a [super-chat](https://github.com/Super-Solution/super-chat) chat app in front of a real SuperRouter server, so you can watch what happens to the money on every message: API keys, credits, reservations, settlement, fallback and the failure cases.

Nothing is sent to a paid model provider. The two "providers" are local mocks; the server, PostgreSQL ledger, HTTP API and API keys are the real thing.

## What's inside

| Path | Port | What it is |
| --- | --- | --- |
| `apps/router` | 4400 | SuperRouter: `createRouterServer` with database-backed API keys, a `chat` Service and two mock providers (`mock-primary`, `mock-backup`) |
| `apps/console` | 4420 | Developer console. Its server holds the workspace owner key and uses the router's admin API: customers, credits, API keys, price, provider behaviour, request log |
| `apps/chat` | 4410 | Chat app built with super-chat (`AgentClient`, `Thread`, `Composer`). Its server holds a customer API key and calls `POST /v1/inferences` |
| `packages/shared` | | Ports, Service ids, credit formatting, local state and HTTP helpers |
| `scenarios.ts` | | Runs every billing situation against the router and prints a table |

```
browser ──► chat server (4410) ──── customer API key ────► router (4400) ──► mock providers
browser ──► console server (4420) ── owner key, admin API ─┘        │
                                                            PostgreSQL ledger
```

Keys never reach the browser. The chat page sends the key once to its own server, which keeps it in memory behind an HttpOnly cookie. The console page never sees the owner key at all.

## Run it

Requires Node.js 24, npm and Docker. From the repository root:

```sh
npm ci
npm run db:up
npm run demo:superchat
```

`demo:superchat` builds the packages and both front ends, then starts all three servers in one process. On first run it creates a local workspace and writes its identity (workspace id, owner key) to `examples/superchat-billing/.local/state.json`, which is gitignored.

Then:

1. Open the **console** at <http://127.0.0.1:4420>. Two customers exist: `alice` with 5 credits and `bob` with 0.12.
2. Click **Issue API key** next to `alice` and copy the key. It is shown once; the router stores only its hash.
3. Open the **chat app** at <http://127.0.0.1:4410>, paste the key and send a message.
4. Back in the console, the request appears in the log with its attempts, provider cost and what `alice` was charged.

## Things to try

| Try this | What you'll see |
| --- | --- |
| Send a message | Credits are held for the worst case, then settled on the tokens actually used. The receipt shows the charge |
| Console → Provider behaviour → **Fallback**, then send | `mock-primary` refuses before running, `mock-backup` answers. Both attempts are recorded; the customer is charged once |
| **All providers fail** | Nothing is charged and the hold is released |
| **Stream cut** | Part of the answer arrives, then the provider stops without final usage. The request stays **pending** and its credits stay held for reconciliation. It is not treated as free and not retried elsewhere |
| **Outcome unknown** | The provider times out after the request was sent. Same as above: held, pending, not retried |
| Chat → **Resend last (same key)** | The same `Idempotency-Key` and body return the original request (HTTP 303). No second charge |
| Issue a key for `bob` and chat | After about five messages he can't cover the next reservation and the router answers `insufficient_balance` (HTTP 402). Add credits in the console and continue |
| Console → **Revoke** a key | The chat app's next request is rejected and it asks for a new key |
| Console → **Publish new price** | A new Service version. Requests already accepted keep the old price; new ones use the new one |

Or run all of them at once while the example is running:

```sh
npm run scenarios:superchat
```

```
Situation           HTTP  Billing / error       Attempts                                     Balance change (credits)
Normal              200   settled               mock-primary:succeeded                       0.004620
Fallback            200   settled               mock-primary:failed → mock-backup:succeeded  0.004620
All providers fail  200   released              mock-primary:failed → mock-backup:failed     0.000000
Stream cut          200   pending               mock-primary:unknown                         held 0.097280
Outcome unknown     200   pending               mock-primary:unknown                         held 0.097280
Retry, same key     303   replayed              (no new attempt)                             0.004620 total
Same key, new body  409   idempotency_conflict
Bob runs out        402   insufficient_balance
Revoked key         401   invalid_key
```

## How the pieces use SuperRouter

- **Provisioning** (`apps/router/src/index.ts`): `ControlPlane.bootstrap` creates the workspace and its owner key; the router then publishes the `chat` Service, creates an `app` member and the two customers, and grants their starting credits. Every step is idempotent, so restarting is safe.
- **Price**: the customer pays 0.00002 credits per input token and 0.00006 per output token (1 credit = 1,000,000,000 atoms). The mock providers cost $0.20 (primary) and $0.40 (backup) per million input tokens, four times that for output. Routing weighs cost only, so the cheaper primary is tried first.
- **Keys** (`apps/console/server.ts`): `POST /v1/admin/keys` issues an `app` key bound to one customer, scoped to `invoke` and `credits:read` and to the `chat` Service. `POST /v1/admin/keys/:id/revoke` revokes it.
- **Credits**: `POST /v1/admin/credits/grants` adds credits; `GET /v1/admin/customers/:id/balance` and `GET /v1/credits` read them.
- **Chat** (`apps/chat/server.ts`): `GET /v1/me` tells the chat server which customer a pasted key belongs to. Each message is `POST /v1/inferences` with an `Idempotency-Key`; the browser reuses the key when it retries the same conversation.
- **Request log**: `GET /v1/admin/requests` lists recent requests with their attempts, provider cost and settled amounts.
- **Provider behaviour**: `POST /__dev/scenario` exists only in this example's router wrapper, guarded by the owner key. It is not part of the SuperRouter API.

The chat app uses the published `@super-solution/super-chat-core`, `-react` and `-ui` 0.5.0 packages. super-chat's own Super Router binding (`@supersolutions/super-chat-router`) isn't on npm yet, so the chat server calls the HTTP API directly in the same shape.

## Settings and cleanup

Optional environment variables are listed in [.env.example](.env.example). `DATABASE_URL` defaults to the development database from `npm run db:up`.

- Start over with a fresh workspace: stop the example and delete `examples/superchat-billing/.local/state.json`. The old workspace's rows stay in the database but are no longer used.
- The bootstrap owner key expires after 30 days. When it does, the router creates a new local workspace automatically on the next start.
- `npm run db:down` stops the database and keeps its volume.

This is a local development example: servers bind to 127.0.0.1, refuse requests from other origins, and keep sessions in memory. Don't deploy it as is.
