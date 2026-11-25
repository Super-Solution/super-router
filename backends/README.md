# Backends

One folder per language. Each implements the same contract and is tested against shared data: the TypeScript, Go and
Python backends against [`../conformance`](../conformance/README.md), Rust against its own copy in `rust/conformance`.
The Rust copy holds every shared case plus the cases for features the TypeScript reference does not have yet; Go reads
the Rust copy for pricing, contracts, routing and the data plane so it is held to both.

| Folder | Status |
| --- | --- |
| `typescript/` | Reference implementation: server, worker, billing presets |
| `go/` | Data plane: contracts, pricing, routing, `router.Infer`, memory and PostgreSQL stores, OpenAI, Anthropic and Gemini providers (Gemini is Rust and Go only for now) with tool calling and image input, `net/http` server and binary. Also the Rust-only features: custom billing units, the rate card, structured output routing, downgrade policy, label evidence, quality metadata and offering capabilities |
| `rust/` | Data plane with its own schema: contracts, pricing, routing, `Router::infer`, memory and PostgreSQL stores, OpenAI, Anthropic and Gemini providers with tool calling and image input, axum server, `super-router-server`. Also custom billing units, the rate card, structured output routing, downgrade policy, label evidence, quality metadata and offering capabilities, which TypeScript and Python do not have yet (Go does) |
| `python/` | Data plane: contracts, pricing, routing, `Router.infer`, memory and PostgreSQL stores, OpenAI and Anthropic providers (text only: no tool calling yet), ASGI server and `super-router-server` |

Go and Python are real servers for the data plane (routing, metering, reserve and settle, provider adapters, swappable
storage). Payments and the control plane (members, keys, plans, subscriptions) exist only in TypeScript for now.

## What each language has (checked against the code at the commit that last edited this table)

`yes` is code plus tests in this repository; none of it has been run against a live upstream (fixtures and fake upstreams only).
"Not yet" is a known gap, not a decision: the TypeScript reference is meant to lead, and the lag is listed in
[`../docs/engineering/improvement-log.md`](../docs/engineering/improvement-log.md) and the conformance README.

| Capability | TypeScript (reference) | Go | Python | Rust |
| --- | --- | --- | --- | --- |
| Contracts, `charge`, `bound`, routing, reserve / settle, replay | yes | yes | yes | yes |
| Stores | memory, PostgreSQL | memory, PostgreSQL | memory, PostgreSQL | memory, PostgreSQL |
| Providers | OpenAI Responses, Anthropic Messages | OpenAI, Anthropic, Gemini | OpenAI, Anthropic | OpenAI, Anthropic, Gemini |
| Tool calling and image input | yes | yes | not yet (refused before reserving) | yes |
| Structured output (`response_format`) | not yet | yes (Gemini) | not yet | yes (Gemini) |
| `RateCard`, `RateSchedule`, `RateTiers` | not yet | yes | not yet | yes |
| Failure classification (`FailureClass`) | not yet | yes (Gemini) | not yet | yes (Gemini) |
| `Retry-After` read into the health book | not yet | yes | not yet | yes |
| `HealthBook` (offering cooldowns, off by default) | not yet | library; not wired into the binary | not yet | library; not wired into the binary |
| Custom billing units, downgrade policy, label evidence, quality metadata | not yet | yes | not yet | yes |
| Usage windows | yes | yes | yes | yes |
| Control plane (members, keys, plans, subscriptions), payments, recovery worker | yes | not yet | not yet | not yet (database-backed keys are read, not created) |
| Shipped binary | `apps/server` (demo) | `super-router-server` | `super-router-server` | `super-router-server` (`super-router-cli`, not published) |
| Published | 13 npm packages | not published | not published | 11 crates on crates.io |

Python is deliberately allowed to lag Go and Rust; the lag is documented here, not hidden.
