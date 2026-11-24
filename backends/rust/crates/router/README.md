# super-router-router

The Super Router data plane: idempotent admission, reserve, execute and settle.

```toml
[dependencies]
super-router-router = "0.3"
```

`Router::infer` admits a request once per idempotency key, routes it, reserves funds, calls the provider as a stream
and settles the exact charge. Storage is a port: implement `Store` / `DataStore`, or use
[`super-router-store-memory`](https://crates.io/crates/super-router-store-memory) or
[`super-router-store-postgres`](https://crates.io/crates/super-router-store-postgres). Models are
`super_router_contracts::Provider`s. `MockProvider` and `MockTaskProvider` are deterministic providers for tests.

Output already shown to the caller, or a lost outcome, keeps the hold for reconciliation and is never refunded.
Settlement outlives the caller: every storage call after funds are reserved runs to completion even if the caller
goes away. `Authenticator` is the port for credentials.

## Status

Version 0.x: a minor release may break the API, a patch release only fixes bugs. All Rust crates of Super Router are
released together at one version. The crate is part of the Rust backend in
[Super-Solution/super-router](https://github.com/Super-Solution/super-router/tree/main/backends/rust), which documents the design, the conformance data and how the crates fit
together. API reference: [docs.rs/super-router-router](https://docs.rs/super-router-router).

## License

Apache-2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE).
