# super-router-routing

Offering selection for Super Router: hard limits, weighted scoring and ordering.

```toml
[dependencies]
super-router-routing = "0.2"
```

`route(&Service, &Invocation)` drops offerings that break a hard limit, scores the rest with the service's weights and orders them by score, then by id. It reproduces the TypeScript
reference including the exact IEEE-754 scores, so offering ids must sort bytewise the same (use `[a-z0-9]`).
`route_at` takes the clock explicitly for tests, and `invocation_bound` is the most one attempt can cost on a price,
the same figure the router reserves.

## Status

Version 0.x: a minor release may break the API, a patch release only fixes bugs. All Rust crates of Super Router are
released together at one version. The crate is part of the Rust backend in
[Super-Solution/super-router](https://github.com/Super-Solution/super-router/tree/main/backends/rust), which documents the design, the conformance data and how the crates fit
together. API reference: [docs.rs/super-router-routing](https://docs.rs/super-router-routing).

## License

Apache-2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE).
