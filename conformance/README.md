# Conformance data

Language-neutral test data for the Super Router core. The TypeScript implementation in
`packages/core` is the reference; every other backend must reproduce these cases exactly.

| File | What it pins |
| --- | --- |
| `pricing.v1.json` | `charge` (customer charge for a usage record), `bound` (amount reserved before a call), `compile_credit_price` (exact units-per-credit rates into the wire price), including validation and error codes |

Atoms and totals are decimal strings, so no language needs a float or a fixed-width integer.

```bash
npx tsx scripts/export-conformance.ts          # regenerate from the TypeScript reference
npx tsx --test tests/conformance.test.ts       # TypeScript still matches the file
(cd backends/go && go test ./...)              # Go port
(cd backends/python && python -m unittest discover -s tests)   # Python port
```

If pricing behaviour changes on purpose, regenerate, review the diff, and update every port.
The Rust backend is closed source and lives in a private repository; it tests against this same file.
