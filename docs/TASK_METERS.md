# Bounded simulated task metering

The invocation engine supports chat tokens plus image counts (`images`), video duration (`video_ms`), audio duration (`audio_ms`), and successful task counts (`tasks`). Durations are integer milliseconds; 1 second is 1000 units. JSON numbers must be nonnegative safe integers. Existing token contracts and token pricing remain compatible.

A task invocation sends `input: {task: {kind: "video", prompt: "Demo"}}` and `limits: {max_usage: {video_ms: 5000}}`. The service and offering both declare `task_kind: "video"`; the offering declares `max_usage: {video_ms: 60000}`. Other kinds are `image`, `audio`, and `task`, with their corresponding single meter. Generic tasks allow exactly `tasks: 1` as the upper bound. Mixing messages with tasks or multiple billable meter kinds is rejected. The server does not accept final usage from the caller.

Task prices keep the existing required token rates (set these to zero for task services) and add independent exact rational rates:

```json
{"asset":"credit:demo","input":"0","cached":"0","cache_write":"0","output":"0","per":1,"media":{"video_ms":{"atoms":"3","per":1000}}}
```

This charges 3 atoms per second; 1500ms costs ceil(4.5) = 5 atoms. Each media component rounds upward once independently; token components retain their shared numerator and single rounding. Monetary quantities use bigint internally and decimal integer strings on the wire, at the existing scale 9. A nonzero unpriced meter is rejected, never silently free.

Routing checks task kind, offered maximum, provider cost and availability before dispatch. Reservations cover the full requested upper bound on each billing leg. `max_charge` must cover that reservation, even if a partial result might later cost less. Only provider adapter events can establish final usage. Malformed, wrong-kind or over-cap usage becomes unknown with no confirmed usage, retains its hold and persists an unknown attempt so recovery cannot settle it. Known partial image/duration results bill their completed units; a partial generic task records zero successful tasks. Unknown completion and missing usage require reconciliation. Definitive rejection releases the reservation.

`MockTaskProvider` is an explicit simulation adapter. It validates bounds, returns a textual simulated receipt and reports adapter-owned completed units. It does **not** generate images, video, audio, URLs or downloadable media. Its success mode completes the bounded requested quantity; partial mode completes half (rounded down), or zero successful generic tasks. The OpenAI and Anthropic adapters remain text-only and reject task input. Production media adapters, external task polling, binary storage, asynchronous callbacks, arbitrary workflows and task cancellation guarantees are outside this slice.

The existing `POST /v1/inferences` transport, idempotency, ledger, traces and worker recovery apply unchanged. See `tests/task-meters.test.ts` for PostgreSQL-backed success, partial, rejection, unknown, bounds, exact pricing and crash recovery coverage.
