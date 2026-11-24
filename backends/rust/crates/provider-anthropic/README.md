# super-router-provider-anthropic

Anthropic Messages API provider for Super Router.

```toml
[dependencies]
super-router-provider-anthropic = "0.3"
```

`Anthropic` implements `super_router_contracts::Provider`: it streams text, normalises usage into router meters and
classifies failures exactly like the OpenAI provider. Redirects are never followed, keys never appear in errors and a
completed message whose usage is missing or malformed is refused rather than billed as free.

```rust
let provider = super_router_provider_anthropic::Anthropic::builder(std::env::var("ANTHROPIC_API_KEY")?).build()?;
```

## Status

Version 0.x: a minor release may break the API, a patch release only fixes bugs. All Rust crates of Super Router are
released together at one version. The crate is part of the Rust backend in
[Super-Solution/super-router](https://github.com/Super-Solution/super-router/tree/main/backends/rust), which documents the design, the conformance data and how the crates fit
together. API reference: [docs.rs/super-router-provider-anthropic](https://docs.rs/super-router-provider-anthropic).

## License

Apache-2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE).
