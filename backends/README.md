# Backends

One folder per language. Each implements the same contract and is tested against shared data: the TypeScript, Go and
Python backends against [`../conformance`](../conformance/README.md), Rust against its own copy in `rust/conformance`.

| Folder | Status |
| --- | --- |
| `typescript/` | Reference implementation: server, worker, billing presets |
| `go/` | Data plane: contracts, pricing, routing, `router.Infer`, memory and PostgreSQL stores, OpenAI and Anthropic providers with tool calling and image input, `net/http` server and binary |
| `rust/` | Data plane with its own schema: contracts, pricing, routing, `Router::infer`, memory and PostgreSQL stores, OpenAI and Anthropic providers with tool calling and image input, axum server, `super-router-server`. Also custom billing units, durable tasks and session/weekly usage windows, which the other backends do not have yet |
| `python/` | Data plane: contracts, pricing, routing, `Router.infer`, memory and PostgreSQL stores, OpenAI and Anthropic providers (text only: no tool calling yet), ASGI server and `super-router-server` |

Go and Python are real servers for the data plane (routing, metering, reserve and settle, provider adapters, swappable
storage). Payments and the control plane (members, keys, plans, subscriptions) exist only in TypeScript for now.
