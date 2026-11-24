# super-router-provider-openai

OpenAI Responses API provider for Super Router.

```toml
[dependencies]
super-router-provider-openai = "0.3"
```

`OpenAi` implements `super_router_contracts::Provider`: it streams text, normalises usage into router meters and
classifies failures. A definitive rejection (the provider certainly did no work) lets the router try the next
offering; anything uncertain keeps the reservation pending.

Redirects are never followed, API keys never appear in errors or `Debug` output and response bodies are never echoed.
A completed response whose usage is missing or malformed is refused rather than billed as free.

```rust
let provider = super_router_provider_openai::OpenAi::builder(std::env::var("OPENAI_API_KEY")?).build()?;
```

## Status

Version 0.x: a minor release may break the API, a patch release only fixes bugs. All Rust crates of Super Router are
released together at one version. The crate is part of the Rust backend in
[Super-Solution/super-router](https://github.com/Super-Solution/super-router/tree/main/backends/rust), which documents the design, the conformance data and how the crates fit
together. API reference: [docs.rs/super-router-provider-openai](https://docs.rs/super-router-provider-openai).

## License

Apache-2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE).
