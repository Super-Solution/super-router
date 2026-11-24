# Gemini provider, and how a new upstream plugs in

Status: implemented in Rust (`super-router-provider-gemini`) and Go (`provider/gemini`); TypeScript and Python later.
Not yet run against live Gemini (no key was available while writing it): everything below is checked against a fake
upstream and the shared conformance file only.

## Where things live

An upstream touches four layers. Keeping them apart is what lets one new upstream be one new crate.

| Layer | Owns | Does not own |
| --- | --- | --- |
| **Provider** (`provider-*`) | The wire dialect: request body, stream parsing, tool calls, failure classification, and **normalising the usage fields into `Meters`** | Prices, rounding, reservations |
| **Pricing** (`pricing`) | Money: `Price`, `charge`, `bound`, `RateCard`, `RateSchedule` (exact integers, round up once) | What a provider's fields mean |
| **Router** | Reserve before, settle after, fallback between offerings | Any dialect |
| **Application** | Its own prices, plans, caps | Dialects (it picks a provider) |

Usage normalisation belongs in the provider because only the provider knows what its fields mean. Everything after it
sees the same four disjoint meters: `input` (uncached), `cached`, `cache_write`, `output`.

## Request mapping

| Router | Gemini `generateContent` |
| --- | --- |
| `system` messages (joined with `\n`) | `systemInstruction.parts[0].text` |
| `assistant` / `user` | role `model` / `user`, `parts: [{text}]` |
| assistant tool call (`arguments` JSON string) | `functionCall {name, args}`; `args` must be an object, `""` is `{}` |
| `tool` message | `functionResponse {name, response: {output \| error: content}}`; the name is found from the call id, a result for a call never made is refused. The results of one step share one user turn |
| tools | `tools[0].functionDeclarations`, schema as `parametersJsonSchema` (JSON Schema as written; the older `parameters` accepts only an OpenAPI subset and would need a lossy translator) |
| `tool_choice` | `toolConfig.functionCallingConfig`: `auto` → `AUTO`, `none` → `NONE`, `required` → `ANY`, `{name}` → `ANY` + `allowedFunctionNames` |
| image part | `inlineData {mimeType, data}` for `data:` URLs. A link is refused (`request_invalid`): Gemini fetches only its own file URIs |
| `max_output_tokens` | `generationConfig.maxOutputTokens` |

Everything the upstream would reject (bad model name, non-object arguments, unknown tool result, link image) is refused
as a definitive `request_invalid` before anything is sent, so the router tries the next offering and nothing is billed.
The model name goes into the path, so only `[A-Za-z0-9._-]` is accepted (a `models/` prefix is dropped).

## Stream mapping

`POST {base}{prefix}/models/{model}:streamGenerateContent?alt=sse`. Each frame is a chunk with `candidates[0]`,
`usageMetadata` (running totals) and `responseId`.

- text parts → `Delta`; thought summaries (`thought: true`) are not forwarded.
- a `functionCall` part arrives whole, not in pieces → one `ToolCall` event with id, name and the arguments. A call
  without an `id` gets `{responseId}_{index}`; one without a name is refused rather than invented.
- `finishReason` ends the answer; `STOP` is `succeeded`, anything else (`MAX_TOKENS`, `SAFETY`, a blocked prompt) is
  `partial`. The stream is read to its end because the last chunk holds the final usage; no `finishReason` is
  `stream_interrupted`, no `usageMetadata` is `usage_unknown`, an unusable one `invalid_usage`. A call is never billed
  as free.
- an `error` frame is `upstream_incomplete`; HTTP 400/401/403/404/422 are definitive, 429 definitive and retryable,
  anything else uncertain (the reservation stays pending), as for the other providers.

## `usageMetadata` → `Meters`

| Gemini field | Meters |
| --- | --- |
| `promptTokenCount − cachedContentTokenCount` | `input` |
| `toolUsePromptTokenCount` (results of built-in tools, billed as input) | added to `input` |
| `cachedContentTokenCount` | `cached` |
| `candidatesTokenCount + thoughtsTokenCount` | `output` |
| `totalTokenCount − (everything above)`, if positive | added to `output` (counted by the provider, so priced; never free) |
| — | `cache_write` stays 0 |

Worked example: prompt 1000, cached 400, candidates 50, thoughts 30 → `input 600, cached 400, output 80`.
Refused as `invalid_usage`: no prompt count, a negative count, more cached than prompt, an overflow. Only the prompt
count is required: a blocked prompt has no candidates count.

**Does `Meters` need a `reasoning` cell?** Not now. Gemini, Anthropic and OpenAI all bill thinking at the output rate,
so a separate cell would be bookkeeping nobody prices differently, and it would touch every backend, the store and the
wire `Price`. The place that can price reasoning separately is `RateCard` (an application with its own price list).
Revisit when a provider prices thinking apart: then `reasoning` (and `cache_write_1h`, the per-call fee) grow into
`Meters`/`Price` together, in one change across the contracts of every backend. Until then the provider folds thinking
into `output`; an application that wants the split reads it from its own copy of the usage.

Why `cache_write` is 0: implicit caching writes for free; explicit caches (`cachedContents`) bill **storage by the
hour**, which is not a token meter. If that has to be billed, it is a separate unit (custom billing units exist for
this) and not a bent token cell.

## Variants: Vertex AI, Bedrock and the rest

Separate the **dialect** (request body, stream, usage) from the **endpoint and authorisation**.

| Upstream | Dialect | Endpoint / auth | How it plugs in |
| --- | --- | --- | --- |
| Gemini API | generateContent | `/v1beta`, `x-goog-api-key` | `Gemini::builder(key)` |
| Vertex AI (Gemini) | generateContent | `/v1/projects/P/locations/L/publishers/google`, bearer OAuth token | same provider: `path_prefix(..)` and `bearer_auth()`; the key source returns the token |
| Azure OpenAI | OpenAI | deployment path, `api-key` header | `OpenAi` needs the same two options (path prefix, header name): small follow-up |
| Anthropic on Vertex | Anthropic Messages | `…/publishers/anthropic/models/M:streamRawPredict`, bearer | `Anthropic` needs the same options; the body differs slightly (`anthropic_version` in the body, no `model`): a variant flag |
| Anthropic / Llama on Bedrock | Converse or Anthropic body | AWS SigV4, **binary event-stream** framing (not SSE) | a new crate: its own framing and signing; usage is disjoint like Anthropic (`inputTokens`, `cacheReadInputTokens`, `cacheWriteInputTokens`), so normalising is a four-line function |

Rule: a variant that only changes where the request goes and how it is authorised is a builder option, not a crate.
A variant with different framing or different counting semantics is a new crate that implements `Provider` and
brings its own `normalize` and its own conformance cases. Nothing else changes.

### Worked example: a new upstream whose cache semantics differ

Say provider X reports `input_tokens` **excluding** cache reads, `cache_read_tokens`, and `cache_write_tokens`
separately (like Anthropic), over SSE. In Rust:

1. `backends/rust/crates/provider-x/` (copy the shape of `provider-gemini`): `src/lib.rs` (request, stream),
   `src/usage.rs` with `normalize(&Usage) -> Option<Meters>`: `input = input_tokens`, `cached = cache_read_tokens`,
   `cache_write = cache_write_tokens`, `output = output_tokens`, refused if any is missing or negative.
2. `backends/rust/Cargo.toml`: one `[workspace.dependencies]` line.
3. `backends/rust/crates/cli/{Cargo.toml,src/lib.rs}`: register `"x"` when `X_API_KEY` is set.
4. `backends/rust/conformance/provider-x.v1.json` (request bodies and recorded streams, from a script like
   `scripts/export-gemini-conformance.mjs`) and a three-line test that calls `run_tool_cases_in`.
5. Go: `backends/go/provider/x/` and one `providertest.RunFile` call; register in `cmd/super-router-server/main.go`.

It does **not** touch `contracts`, `pricing`, `routing`, `router`, the stores or any other provider. The offering that
uses it names `"provider": "x"`.

## What is not covered, and what comes next

- **Thought signatures (blocks multi-turn tool use on Gemini 3 models).** Gemini 3 returns a `thoughtSignature` on
  `functionCall` parts and, as I understand Google's documentation, rejects the next turn if it is not sent back. The
  contracts' `ToolCall` has no field to carry it. Proposed: an optional opaque `signature` on `ToolCall` and on the
  `ToolCall` provider event, echoed by the provider that produced it. It is additive but touches the contracts of every
  backend, so it is its own change. Verify against a live key first.
- Not run against live Gemini. First step once a key exists: replay one recorded real stream into the conformance file.
- Structured output (`response_format`): no provider writes the schema yet (router-wide, 422 after routing).
- Generation parameters beyond `maxOutputTokens` (temperature, safety settings), grounding tools, audio and video
  parts, explicit-cache creation: not in the contracts.
- Tool-call arguments are re-encoded as compact JSON with sorted keys. Number spelling differs by backend: Go keeps
  `1e3`, Rust writes `1000.0`. Both parse to the same value; the conformance cases use plain integers.
- TypeScript and Python adapters: later, from the same conformance file.

## Order of work

In this PR (separate commits): (1) Rust and Go Gemini providers with the shared conformance file, (2) `RateSchedule`
and `RateCard::defaulted()`, which are what FirstFa's pricing incident asked for: a price that changes on a date, and a
warning when usage counted cache reads from a card that has no cache-read rate.

Next, each small and separable: (3) `ToolCall.signature` in the contracts, (4) path-prefix and auth options on the
OpenAI and Anthropic providers (Azure, Vertex Anthropic), (5) decide whether the wire `Price` grows `reasoning`,
`cache_write_1h` and a per-call fee, (6) TypeScript and Python parity, (7) application side: FirstFa swaps its own
`accounting::price` for `RateCard` after the next release and gets Gemini either from this provider or by adding the
protocol to its own gateway; its protocol table needs a call path that can carry the model name.

Release: a new crate. The first publish of `super-router-provider-gemini` must happen with a token before Trusted
Publishing can be configured for it (`docs/CRATES-PUBLISHING.md` lists ten crates; it becomes eleven at 0.3.0).
