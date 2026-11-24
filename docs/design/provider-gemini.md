# Gemini provider, and how a new upstream plugs in

Status: implemented in Rust (`super-router-provider-gemini`) and Go (`provider/gemini`); TypeScript and Python later.
Not yet run against live Gemini (no key was available while writing it): everything below is checked against a fake
upstream and the shared conformance file only.

## Where things live

An upstream touches four layers. Keeping them apart is what lets one new upstream be one new crate.

| Layer | Owns | Does not own |
| --- | --- | --- |
| **Provider** (`provider-*`) | The wire dialect: request body, stream parsing, tool calls, failure classification, and **normalising the usage fields into `Meters`** | Prices, rounding, reservations |
| **Pricing** (`pricing`) | Money: `Price`, `charge`, `bound`, `RateCard`, `RateSchedule`, `RateTiers` (exact integers, round up once) | What a provider's fields mean |
| **Router** | Reserve before, settle after, fallback between offerings | Any dialect |
| **Application** | Its own prices, plans, caps | Dialects (it picks a provider) |

Usage normalisation belongs in the provider because only the provider knows what its fields mean. Everything after it
sees the same four disjoint meters: `input` (uncached), `cached`, `cache_write`, `output`.

## Request mapping

| Router | Gemini `generateContent` |
| --- | --- |
| `system` messages (joined with `\n`) | `systemInstruction.parts[0].text` |
| `assistant` / `user` | role `model` / `user`, `parts: [{text}]` |
| assistant tool call (`arguments` JSON string) | `functionCall {name, args}`; `args` must be an object, `""` is `{}`. A call's `signature`, when it has one, goes beside it as `thoughtSignature`, unchanged |
| `tool` message | `functionResponse {name, response: {output \| error: content}}`; the name is found from the call id, a result for a call never made is refused. The results of one step share one user turn |
| tools | `tools[0].functionDeclarations`, schema as `parametersJsonSchema` (JSON Schema as written; the older `parameters` accepts only an OpenAPI subset and would need a lossy translator). A schema that is just an empty object (no properties, no other keywords) declares no schema at all: that is how Gemini says "no parameters", and an empty object schema is a request some models refuse |
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
  without an `id` gets `{responseId}_{index}`; one without a name is refused rather than invented. A
  `thoughtSignature` on that part becomes the event's `signature` (an empty one is dropped); a signature on a text
  part is not a call signature and is ignored.
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

A provider crate does **not** touch `pricing`, `routing`, `router`, the stores or any other provider (the one change to `contracts` in this PR, the call signature, is its own commit). The offering that
uses it names `"provider": "x"`.

## What is not covered, and what comes next

- **Thought signatures: carried, not yet seen live.** Gemini 3 returns a `thoughtSignature` on `functionCall` parts
  and, as I understand Google's documentation, rejects the next turn if a call from the current turn is replayed
  without it. `ToolCall` and the `ToolCall` provider event now have an optional opaque `signature` (1 to 16384
  characters, no whitespace; the router puts it on the first delta and on `output.tool_call.completed`, the result's
  `tool_calls[]` carries it, and a caller sends the call back unchanged). Rust and Go only, like the other
  additions beyond the TypeScript reference, which refuses the unknown key until it follows. Not carried: signatures
  on text parts (Google recommends, but does not require, sending them back) and a replayed history that never had
  signatures (another model's turns). For that case Google documents placeholder values; using them is a policy
  choice for the application, not something the provider should do silently.
- Not run against live Gemini. First step once a key exists: replay one recorded real stream into the conformance file.
- Structured output (`response_format`): no provider writes the schema yet (router-wide, 422 after routing).
- Generation parameters beyond `maxOutputTokens` (temperature, thinking level or budget, safety settings), grounding
  tools, audio and video parts, explicit-cache creation: not in the contracts. Safety settings and thinking controls
  are the ones real traffic will ask for first; they are application policy, so they should arrive as optional
  provider options, not as FirstFa-specific fields.
- Modality-specific prices (Gemini bills audio input higher than text) need the per-modality token counts in
  `usageMetadata.promptTokensDetails`; `Meters` has one `input` cell, so today the whole prompt is priced alike. A
  card per modality is possible without touching the wire price; it is not built.
- Tool-call arguments are re-encoded as compact JSON with sorted keys. Number spelling differs by backend: Go keeps
  `1e3`, Rust writes `1000.0`. Both parse to the same value; the conformance cases use plain integers.
- TypeScript and Python adapters: later, from the same conformance file.

## What other gateways do, and what was taken from them

Read for ideas and edge cases only, never copied: a New API fork (clawRouter, same lineage as New API itself; the
upstream project was not fetched) and LiteLLM 1.110 (the published package). Both are older and broader than this
library; the useful parts were few and specific.

**Adopted**

| Seen in | What | Where it landed |
| --- | --- | --- |
| LiteLLM | `candidatesTokenCount` sometimes already contains the thoughts (told apart by `prompt + candidates + toolUse == total`); adding `thoughtsTokenCount` again bills them twice | `usage.rs` / `usage.go`, with an oracle case |
| LiteLLM | A thought signature only validates on the backend that minted it; the Gemini API and Vertex answer each other's with `400 Corrupted/Invalid thought signature` | the distinct definitive code `thought_signature_rejected`, so a conversation that moved backends says why it failed |
| LiteLLM | Long-context prices: every rate of a call rises when the prompt passes 200,000 tokens, output and cache included | `RateTiers` (Rust + Go), exact integers, a hold that covers every reachable tier |
| New API (via clawRouter) | A model with no cache-read rate is billed at the full input rate by default, and that has caused real overcharges; one bad value in the rate blob wipes every model's rates | already what `defaulted()` and a whole-card validation are for; the cases confirm the design rather than change it |
| both | An empty-object tool schema is a request some Gemini models refuse | no-parameter tools declare no schema |
| LiteLLM | Replaying the same signature on a text part and on its call part bills the previous turn's reasoning twice | not an issue here: text-part signatures are not sent at all |

**Deliberately not adopted**

- **Floating-point prices** (`3.1248e-07` dollars per token) and a mutable global price table. Prices stay exact
  integers on a value the application owns.
- **A field per modality and per service tier** (audio input, image output, cache-read audio, priority, flex and batch
  variants: LiteLLM has dozens of price fields per model). One card prices six kinds; a modality or service tier is a
  card of its own that the application selects. If real traffic needs per-modality prices, they become more cards, not
  more fields on one.
- **A placeholder signature after a rejected replay** (resend with Google's `skip_thought_signature_validator`). It
  silently drops the reasoning continuity, so it is a policy for the application, not a default of the provider.
- **Continuation hops after `MAX_TOKENS`**, a filler `"."` user message when only a system message is sent, and
  stripping unsupported schema keywords. The first changes billing and output boundaries, the second invents content,
  the third is unnecessary with `parametersJsonSchema`; a system-only request is refused instead.
- **Search-grounding rules** (tool-use tokens free when a search ran, per-query fees): no grounding tool is exposed.
- **A price floor of 1 quota unit** on any call with tokens: rounding up once already makes any usage with a positive
  price at least one atom; a rate of `"0"` is a decision, not a collapse.

LiteLLM's way to add a provider (a config object per provider that builds the URL, the headers and the body, with the
price in a data file) is the same split as here: the provider crate owns the dialect, the price lives with the
application. Two places where it is looser: pricing sits in a shared file that every deployment reads, and usage is
normalised into one OpenAI-shaped record that has to grow a field per provider quirk.

## Order of work

In this PR (separate commits): (1) Rust and Go Gemini providers with the shared conformance file, (2) `RateSchedule`
and `RateCard::defaulted()`, which are what FirstFa's pricing incident asked for: a price that changes on a date, and a
warning when usage counted cache reads from a card that has no cache-read rate.

Also in this PR: (3) `ToolCall.signature` in the contracts (Rust and Go), the empty-parameters rule and their
conformance cases, from checking the adapter against how a production gateway handles Gemini edge cases (usage with
tool-use prompt tokens, blocked prompts, empty schemas, signatures).

Also in this PR: (4) `RateTiers`, the folded-thoughts usage rule and `thought_signature_rejected`, from reading the
other gateways (next section).

Next, each small and separable: (5) path-prefix and auth options on the
OpenAI and Anthropic providers (Azure, Vertex Anthropic), (6) decide whether the wire `Price` grows `reasoning`,
`cache_write_1h` and a per-call fee, (7) TypeScript and Python parity, (8) application side: FirstFa swaps its own
`accounting::price` for `RateCard` after the next release and gets Gemini either from this provider or by adding the
protocol to its own gateway; its protocol table needs a call path that can carry the model name.

Release: a new crate. The first publish of `super-router-provider-gemini` must happen with a token before Trusted
Publishing can be configured for it (`docs/CRATES-PUBLISHING.md` lists ten crates; it becomes eleven at 0.3.0).
