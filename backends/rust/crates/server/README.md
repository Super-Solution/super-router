# super-router-server

The Super Router data plane HTTP API as an axum Router.

```toml
[dependencies]
super-router-server = "0.5"
```

`handler(Options)` returns a plain `axum::Router` that mounts on any axum, hyper or tower stack. Authentication is the
`Authenticator` port and storage is `DataStore`; `StaticKeys` authenticates against a fixed list of keys by constant-time digest comparison.
`GET /healthz` needs no credentials, everything else needs a bearer token and every `POST` needs an `Idempotency-Key`.
Routes, responses and error codes match the Go and TypeScript reference servers, and an unknown path or wrong method
is always the same `resource_not_found` JSON error.

## Status

Version 0.x: a minor release may break the API, a patch release only fixes bugs. All Rust crates of Super Router are
released together at one version. The crate is part of the Rust backend in
[Super-Solution/super-router](https://github.com/Super-Solution/super-router/tree/main/backends/rust), which documents the design, the conformance data and how the crates fit
together. API reference: [docs.rs/super-router-server](https://docs.rs/super-router-server).

## License

Apache-2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE).
