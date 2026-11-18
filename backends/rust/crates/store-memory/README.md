# super-router-store-memory

In-memory DataStore for Super Router: tests, demos and single-process self-hosting.

```toml
[dependencies]
super-router-store-memory = "0.2"
```

`MemoryStore` follows the PostgreSQL store's semantics exactly: idempotent admission, bucketed holds, a provider cost
budget, a balanced journal and an append-only outbox. One mutex makes every method atomic. State is lost on restart,
so use it for tests, demos and single-process self-hosting; use
[`super-router-store-postgres`](https://crates.io/crates/super-router-store-postgres) when state must survive.

## Status

Version 0.x: a minor release may break the API, a patch release only fixes bugs. All Rust crates of Super Router are
released together at one version. The crate is part of the Rust backend in
[Super-Solution/super-router](https://github.com/Super-Solution/super-router/tree/main/backends/rust), which documents the design, the conformance data and how the crates fit
together. API reference: [docs.rs/super-router-store-memory](https://docs.rs/super-router-store-memory).

## License

Apache-2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE).
