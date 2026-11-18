# super-router-pricing

Exact big-integer pricing for [Super Router](https://github.com/Super-Solution/super-router): `charge`, `bound` and `compile_credit_price`.

```toml
[dependencies]
super-router-pricing = "0.2"
```

Money is integer atoms held in `BigInt` and decimal strings, never floats, so every result matches the TypeScript
reference bit for bit.

- `charge(&Usage, &Price)` prices measured usage, `bound(input, output, &Price)` is the upper bound reserved before a
  call: the dearest input rate times `input`, plus `output`.
- `Price` covers token, media and custom-unit rates; `compile_credit_price` turns a credit rate into a `Price`.
- `atoms`, `valid_atoms`, `valid_id` and `valid_unit_name` validate untrusted input.
- `CREDIT_ATOMS` is the size of one credit in atoms.

## Status

Version 0.x: a minor release may break the API, a patch release only fixes bugs. All Rust crates of Super Router are
released together at one version. The crate is part of the Rust backend in
[Super-Solution/super-router](https://github.com/Super-Solution/super-router/tree/main/backends/rust), which documents the design, the conformance data and how the crates fit
together. API reference: [docs.rs/super-router-pricing](https://docs.rs/super-router-pricing).

## License

Apache-2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE).
