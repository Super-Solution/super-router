# Backends

One folder per language. Each implements the same contract and is tested against the shared data in
[`../conformance`](../conformance/README.md).

| Folder | Status |
| --- | --- |
| `typescript/` | Reference implementation: server, worker, billing presets |
| `go/` | Proof of concept: the pricing core (`charge`, `bound`, `CompileCreditPrice`) |
| `python/` | Proof of concept: the same pricing core |

The proofs of concept cover exact money arithmetic and rate validation only. They have no HTTP server,
storage or provider adapters yet.
