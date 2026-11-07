# Backends

One folder per language. Each implements the same contract and is tested against the shared data in
[`../conformance`](../conformance/README.md).

| Folder | Status |
| --- | --- |
| `typescript/` | Reference implementation: server, worker, billing presets |
| `go/` | Data plane: contracts, pricing, routing, `router.Infer`, memory and PostgreSQL stores, OpenAI and Anthropic providers, `net/http` server and binary |
| `python/` | Data plane: contracts, pricing, routing, `Router.infer`, memory and PostgreSQL stores, OpenAI and Anthropic providers, ASGI server and `super-router-server` |

Go and Python are real servers for the data plane (routing, metering, reserve and settle, provider adapters, swappable
storage). Payments and the control plane (members, keys, plans, subscriptions) exist only in TypeScript for now.
