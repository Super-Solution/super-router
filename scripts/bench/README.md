# Router benchmark harness

Measures what a router adds to a streamed answer, so a change to a hot path (the TCP socket options, the store, the
provider adapter) is judged by numbers instead of by reading. It is a harness, not a gate: nothing in `check-all.sh`
runs it, and nothing in the published packages or crates depends on it.

```bash
scripts/bench/build.sh                                   # builds from a COPY of the backends into /tmp/srbench
DELAY=0 scripts/bench/run.sh                             # the whole matrix, 2 repetitions
TARGETS="rust-nodelay rust-stock direct" CONFIGS="router 1 1 2000" REPS=2 scripts/bench/run.sh
BENCH_REPO=/path/to/another/checkout BENCH_OUT=/tmp/before scripts/bench/build.sh   # before/after: build both, run both
```

`run.sh` documents the targets and the configuration syntax (`MODE STREAM CONCURRENCY REQUESTS`). A fake OpenAI
Responses upstream answers with three SSE events after a fixed delay; the load generator sends one unique
`Idempotency-Key` per request. `build.sh` copies the harness crate into the copy of the Rust workspace, so the
repository's workspace and its published crates are untouched. `pgrun.sh` runs the same matrix on the PostgreSQL store.

## How to read it

- Needs four CPUs: the fake upstream is pinned to CPU 0, the server under test to CPUs 1 and 2, the load generator to CPU 3.
- "Added" is a target's p50 minus `direct`'s p50 (the load generator talking to the fake upstream alone).
- Repeat and compare ranges. The numbers below come from a shared 4 vCPU virtual machine (Intel Xeon @ 2.10 GHz, 16 GB,
  load average 3 to 7 from other work) and are indicative only; a quiet dedicated machine will differ.

## Recorded: `TCP_NODELAY` on accepted connections (Rust)

The Rust server answers a streamed response in small writes. With Nagle's algorithm on (the default for a socket
`axum::serve` accepts), the second small write waits for the client's delayed ACK, about 40 ms on Linux.
`super_router_cli::nodelay` wraps the listener and sets `TCP_NODELAY` on every accepted connection.

Harness router, fake upstream with no delay, streaming, concurrency 1, 2000 requests, 2 repetitions
(`TARGETS="rust-stock rust-nodelay direct"`, 2026-11-26):

| target | p50 | p90 | p99 | requests/s |
| --- | --- | --- | --- | --- |
| rust-stock (Nagle on) | 44.02 / 44.03 ms | 48.01 / 48.03 ms | 52.03 / 56.05 ms | 22 / 22 |
| rust-nodelay | 0.55 / 0.51 ms | 1.15 / 4.49 ms | 5.57 / 21.56 ms | 1250 / 495 |
| direct (the floor) | 0.06 / 0.08 ms | 0.07 / 0.25 ms | 0.28 / 5.38 ms | 13576 / 2895 |

The median falls by about 80 times. The tail of the second repetition is noise from the shared machine (the `direct` row
moved the same way). Non-streaming responses, which are one write, do not change.

The shipped `super-router-server` (`rust-cli`, mock provider, which writes its events together) shows the same effect
only in the tail, because its events are not separate writes: before, with `TCP_NODELAY` off, streaming concurrency 1 had
a p99 of about 43 ms (rps 803 and 941); after, 2 to 3 ms (rps 3603 and 3729). The p50 stayed at 0.18 to 0.21 ms.
At concurrency 64 the two builds are inside each other's noise (8853 to 13407 requests/s before, 10700 to 11498 after).
