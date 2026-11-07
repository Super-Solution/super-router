import test from "node:test";
import assert from "node:assert/strict";
import {
  OpenAIProvider,
  normalizeOpenAI,
} from "@supersolutions/super-router-provider-openai";
import {
  AnthropicProvider,
  normalizeAnthropic,
} from "@supersolutions/super-router-provider-anthropic";
import { parseSSE } from "@supersolutions/super-router-contracts";
import { demoInput, demoService } from "../scripts/demo-config.js";
const offering = demoService.offerings[0]!;
function sse(events: any[]) {
  const text = events
    .map((e) => "data: " + JSON.stringify(e) + "\r\n\r\n")
    .join("");
  const bytes = new TextEncoder().encode(text);
  return new Response(
    new ReadableStream({
      start(c) {
        for (let i = 0; i < bytes.length; i += 7)
          c.enqueue(bytes.slice(i, i + 7));
        c.close();
      },
    }),
  );
}
test("OpenAI normalizes non-overlapping cache meters and streams terminal usage", async () => {
  const raw = {
    input_tokens: 30,
    input_tokens_details: { cached_tokens: 10, cache_write_tokens: 5 },
    output_tokens: 7,
  };
  assert.deepEqual(normalizeOpenAI(raw), {
    input: 15,
    cached: 10,
    cache_write: 5,
    output: 7,
  });
  let request: any;
  const transport: typeof fetch = async (_url, init) => {
    request = JSON.parse(init!.body as string);
    return sse([
      { type: "response.output_text.delta", delta: "你好" },
      { type: "response.completed", response: { id: "up_1", usage: raw } },
    ]);
  };
  const provider = new OpenAIProvider(() => "test-only", transport);
  const events = await Array.fromAsync(
    provider.invoke(demoInput, offering, new AbortController().signal),
  );
  assert.equal(request.store, false);
  assert.equal(events[0]?.type, "delta");
  assert.equal(events.at(-1)?.type, "done");
  assert.equal((events[1] as any).usage.input, 15);
});
test("Anthropic cumulative output counts replace rather than accumulate", async () => {
  assert.deepEqual(
    normalizeAnthropic({
      input_tokens: 10,
      cache_read_input_tokens: 5,
      cache_creation_input_tokens: 3,
      output_tokens: 7,
    }),
    { input: 10, cached: 5, cache_write: 3, output: 7 },
  );
  const transport: typeof fetch = async () =>
    sse([
      {
        type: "message_start",
        message: { id: "msg_1", usage: { input_tokens: 10, output_tokens: 1 } },
      },
      {
        type: "content_block_delta",
        delta: { type: "text_delta", text: "hi" },
      },
      { type: "message_delta", usage: { output_tokens: 3 } },
      { type: "message_delta", usage: { output_tokens: 7 } },
      { type: "message_stop" },
    ]);
  const events = await Array.fromAsync(
    new AnthropicProvider(() => "test-only", transport).invoke(
      demoInput,
      offering,
      new AbortController().signal,
    ),
  );
  assert.equal((events[1] as any).usage.output, 7);
});
test("interrupted provider streams are not successful zero-usage responses", async () => {
  const transport: typeof fetch = async () =>
    sse([{ type: "response.output_text.delta", delta: "partial" }]);
  await assert.rejects(
    () =>
      Array.fromAsync(
        new OpenAIProvider(() => "test-only", transport).invoke(
          demoInput,
          offering,
          new AbortController().signal,
        ),
      ),
    /stream_interrupted/,
  );
});
test("upstream transport errors are unknown, HTTP 429 is definite retryable rejection", async () => {
  const unknown: typeof fetch = async () => {
    throw new Error("socket reset secret material");
  };
  await assert.rejects(
    () =>
      Array.fromAsync(
        new OpenAIProvider(() => "test", unknown).invoke(
          demoInput,
          offering,
          new AbortController().signal,
        ),
      ),
    (e) => (e as any).definitive === false && !String(e).includes("secret"),
  );
  const limited: typeof fetch = async () => new Response("", { status: 429 });
  await assert.rejects(
    () =>
      Array.fromAsync(
        new AnthropicProvider(() => "test", limited).invoke(
          demoInput,
          offering,
          new AbortController().signal,
        ),
      ),
    (e) => (e as any).definitive === true && (e as any).retryable === true,
  );
});
test("OpenAI terminal incomplete with confirmed usage reports billable partial output", async () => {
  const transport: typeof fetch = async () =>
    sse([
      { type: "response.output_text.delta", delta: "partial" },
      {
        type: "response.incomplete",
        response: {
          id: "up_partial",
          usage: { input_tokens: 10, output_tokens: 5 },
        },
      },
    ]);
  const events = await Array.fromAsync(
    new OpenAIProvider(() => "test", transport).invoke(
      demoInput,
      offering,
      new AbortController().signal,
    ),
  );
  assert.equal((events.at(-1) as any).completion, "partial");
  assert.equal((events.at(-2) as any).usage.output, 5);
});
test("both real adapter implementations can share one routed Service using HTTP fixtures", async () => {
  const { Router } = await import("@supersolutions/super-router-core");
  const { PostgresStore, LOCAL_DATABASE_URL } =
    await import("@supersolutions/super-router-storage-postgres");
  const { randomUUID } = await import("node:crypto");
  const { principal, seed, digestKey } =
    await import("../scripts/demo-config.js");
  const store = new PostgresStore(
    process.env.DATABASE_URL ?? LOCAL_DATABASE_URL,
  );
  await store.migrate();
  const p = { ...principal, workspace: "adapters-" + randomUUID() };
  const config = {
    ...demoService,
    offerings: [
      { ...offering, id: "a-openai", provider: "openai" },
      { ...offering, id: "b-anthropic", provider: "anthropic" },
    ],
  };
  await seed(store, p, config);
  let openCalls = 0,
    anthropicCalls = 0;
  const openFetch: typeof fetch = async () => {
    openCalls++;
    return new Response("", { status: 429 });
  };
  const anthFetch: typeof fetch = async () => {
    anthropicCalls++;
    return sse([
      {
        type: "message_start",
        message: { id: "msg", usage: { input_tokens: 10, output_tokens: 1 } },
      },
      {
        type: "content_block_delta",
        delta: { type: "text_delta", text: "fixture reply" },
      },
      { type: "message_delta", usage: { output_tokens: 5 } },
      { type: "message_stop" },
    ]);
  };
  try {
    const router = new Router(
      store,
      {
        openai: new OpenAIProvider(() => "fixture", openFetch),
        anthropic: new AnthropicProvider(() => "fixture", anthFetch),
      },
      digestKey,
    );
    const r = await router.infer(p, demoInput, "both-adapters");
    assert.equal(r.billing_status, "settled");
    assert.equal(openCalls, 1);
    assert.equal(anthropicCalls, 1);
    assert.equal((await store.trace(p, r.id)).attempts.length, 2);
  } finally {
    await store.close();
  }
});
