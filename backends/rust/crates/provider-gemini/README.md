# super-router-provider-gemini

Google Gemini `generateContent` provider for Super Router: the Gemini API and, with two builder calls, Vertex AI.

```toml
[dependencies]
super-router-provider-gemini = "0.2"
```

`Gemini` implements `super_router_contracts::Provider`: it streams text and tool calls, normalises `usageMetadata`
into router meters and classifies failures. A definitive rejection (the provider certainly did no work) lets the
router try the next offering; anything uncertain keeps the reservation pending.

Redirects are never followed, API keys never appear in errors or `Debug` output and response bodies are never echoed.
A completed response whose usage is missing or malformed is refused rather than billed as free.

```rust
// Gemini API (API key in `x-goog-api-key`)
let gemini = super_router_provider_gemini::Gemini::builder(std::env::var("GEMINI_API_KEY")?).build()?;

// Vertex AI: same wire format, a different path prefix and an OAuth access token as a bearer token.
let vertex = super_router_provider_gemini::Gemini::builder_with_key_source(token_source)
    .base_url("https://us-central1-aiplatform.googleapis.com")
    .path_prefix("/v1/projects/my-project/locations/us-central1/publishers/google")
    .bearer_auth()
    .build()?;
```

How Gemini's usage becomes meters, what is not mapped and how to add a variant: see
[docs/design/provider-gemini.md](https://github.com/Super-Solution/super-router/blob/main/docs/design/provider-gemini.md).

## Status

Version 0.x: a minor release may break the API, a patch release only fixes bugs. All Rust crates of Super Router are
released together at one version. The crate is part of the Rust backend in
[Super-Solution/super-router](https://github.com/Super-Solution/super-router/tree/main/backends/rust).

## License

Apache-2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE).
