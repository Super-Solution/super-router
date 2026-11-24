# super-router-contracts

Wire types, strict parsing of untrusted JSON and canonical JSON for [Super Router](https://github.com/Super-Solution/super-router).

```toml
[dependencies]
super-router-contracts = "0.3"
```

Everything the other crates exchange: `Service`, `Offering`, `Invocation`, `Principal`, `Meters`, the `Provider`
trait and its `ProviderEvent` stream, and the error types.

- `parse_service` and `parse_invocation` are the strict entry points for untrusted JSON.
- `canonical` writes the canonical JSON that feeds the idempotency digest, with `js_number` reproducing the
  TypeScript number formatting.
- `parse_utc_millis` and `valid_datetime` handle the RFC 3339 subset the API accepts.

Implement `Provider` to add a model provider; see `super-router-provider-openai` and `super-router-provider-anthropic`.

## Status

Version 0.x: a minor release may break the API, a patch release only fixes bugs. All Rust crates of Super Router are
released together at one version. The crate is part of the Rust backend in
[Super-Solution/super-router](https://github.com/Super-Solution/super-router/tree/main/backends/rust), which documents the design, the conformance data and how the crates fit
together. API reference: [docs.rs/super-router-contracts](https://docs.rs/super-router-contracts).

## License

Apache-2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE).
