# super-router-sse

Bounded server-sent-events reader for Super Router provider adapters.

```toml
[dependencies]
super-router-sse = "0.3"
```

Reads server-sent events across arbitrary chunk and CRLF boundaries. A hostile upstream cannot exhaust memory: one
event may not exceed `MAX_FRAME_BYTES`, including a single line that never ends, and the read is abandoned the moment
the budget is passed. Content is never logged.

`frames(body)` turns any `Stream<Item = Result<impl AsRef<[u8]>, E>>`, such as a reqwest byte stream, into a stream of
`Frame`s. The stream ends after the first error, and dropping it drops the body. `Parser` is the same logic for callers
that push bytes themselves.

## Status

Version 0.x: a minor release may break the API, a patch release only fixes bugs. All Rust crates of Super Router are
released together at one version. The crate is part of the Rust backend in
[Super-Solution/super-router](https://github.com/Super-Solution/super-router/tree/main/backends/rust), which documents the design, the conformance data and how the crates fit
together. API reference: [docs.rs/super-router-sse](https://docs.rs/super-router-sse).

## License

Apache-2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE).
