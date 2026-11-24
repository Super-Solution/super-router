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
