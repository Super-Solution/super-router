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
| Structured output (`response_format`) | yes (OpenAI, Anthropic) | yes (all three) | yes (OpenAI, Anthropic) | yes (all three) |
| `RateCard`, `RateSchedule`, `RateTiers` | not yet | yes | not yet | yes |
| Failure classification (`FailureClass`) of a provider's refusal | yes (OpenAI, Anthropic) | yes (all three) | yes (OpenAI, Anthropic) | yes (all three) |
| `Retry-After` read from a refusal | yes | yes | yes | yes |
| `HealthBook` (offering cooldowns, off by default) | not yet | yes; `SUPERROUTER_HEALTH=on` in the binary | not yet (the binary refuses `on`) | yes; `SUPERROUTER_HEALTH=on` in the binary |
| Provider base URL in the binary (`OPENAI_BASE_URL`, `ANTHROPIC_BASE_URL`, `GEMINI_BASE_URL`) | not applicable (demo server) | yes | yes (OpenAI, Anthropic) | yes |
| Custom billing units, downgrade policy, label evidence, quality metadata | not yet | yes | not yet | yes |
| Usage windows | yes | yes | yes | yes |
| Control plane (members, keys, plans, subscriptions), payments, recovery worker | yes | not yet | not yet | not yet (database-backed keys are read, not created) |
| Shipped binary | `apps/server` (demo) | `super-router-server` | `super-router-server` | `super-router-server` (`super-router-cli`, not published) |
| Published | 13 npm packages | not published | not published | 11 crates on crates.io |

### The shipped binaries: provider hosts, the health book and key pools

`OPENAI_BASE_URL`, `ANTHROPIC_BASE_URL` and `GEMINI_BASE_URL` (Python: the first two) say where a provider's requests go:
the scheme, host and any path prefix, **without** the trailing `/v1` (or `/v1beta`), which the provider adds itself. A value
that ends in one is refused at startup (the official OpenAI SDKs' `OPENAI_BASE_URL` includes `/v1`; ours does not), and a
value is never echoed in an error because a URL can carry credentials. The OpenAI provider speaks the Responses API, so a
host must implement `/v1/responses`; a Chat Completions-only host (vLLM, most gateways) needs the Chat provider, which does
not exist yet.

`SUPERROUTER_HEALTH=on` (Go and Rust) gives the router a `HealthBook`: an offering whose own failure repeats (rejected key,
empty account, retired model, blocked region, rate limit) is skipped until its cooldown ends, then one request probes it.
Without it every request pays one failed attempt on the bad offering before falling through. The cooldowns are the book's
defaults; `SUPERROUTER_HEALTH_*_MS` changes them (see the binary's header comment). The Python binary refuses `on` instead of
ignoring it.

A key is a property of an offering, so a pool of keys for one model is several offerings, each naming a provider that holds
one key. The binaries register one provider per vendor, so today a pool spreads over **different vendors or hosts**
(offering `a` on `openai`, offering `b` on `anthropic`); the end-to-end test in each binary
(`a_rejected_key_is_skipped_only_when_the_health_book_is_on`, `TestARejectedKeyIsSkippedOnlyWhenTheHealthBookIsOn`) shows
a rejected key costing one attempt and then nothing. Two keys on the *same* vendor need the library (one provider instance
per key) until the binaries can register more than one provider per vendor. The Rust and Go copies of the data-plane
scenarios include one that runs with a health book (`health` in the scenario) and shows only the failing offering being
skipped, then probed after its cooldown.

Python is deliberately allowed to lag Go and Rust; the lag is documented here, not hidden.
