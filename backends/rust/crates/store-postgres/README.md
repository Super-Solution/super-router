# super-router-store-postgres

PostgreSQL DataStore for Super Router on its own schema, with database-backed API keys.

```toml
[dependencies]
super-router-store-postgres = "0.4"
```

`PostgresStore` implements `super_router_router::DataStore` on sqlx. Queries are plain runtime SQL (no database is
needed to compile), every write runs in a transaction that first takes the tenant's advisory lock, and the migrations
are embedded (`embedded_migrations`).

**Use a database of its own.** This backend's schema, migration history and billing features (custom units, durable
tasks, attempt failures, session and weekly usage windows) differ from the TypeScript, Go and Python backends in the
same repository. `Keys` and `KeyRevalidator` authenticate bearer tokens against the control plane's keys.

## Status

Version 0.x: a minor release may break the API, a patch release only fixes bugs. All Rust crates of Super Router are
released together at one version. The crate is part of the Rust backend in
[Super-Solution/super-router](https://github.com/Super-Solution/super-router/tree/main/backends/rust), which documents the design, the conformance data and how the crates fit
together. API reference: [docs.rs/super-router-store-postgres](https://docs.rs/super-router-store-postgres).

## License

Apache-2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE).
