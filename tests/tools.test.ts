import test from "node:test";
import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import {
  Invocation,
  invocationNeeds,
  Service,
  type Provider,
} from "@supersolutions/super-router-contracts";
import {
  canonical,
  MockProvider,
  MockToolProvider,
  Router,
} from "@supersolutions/super-router-core";
import { MemoryStore } from "@supersolutions/super-router-storage-memory";
import { createRouterHandler } from "@supersolutions/super-router-server";
import {
  OpenAIProvider,
  toOpenAIInput,
} from "@supersolutions/super-router-provider-openai";
import {
  AnthropicProvider,
  toAnthropicMessages,
} from "@supersolutions/super-router-provider-anthropic";
import {
  demoInput,
  demoService,
  digestKey,
  principal,
  seed,
} from "../scripts/demo-config.js";

const lookup = {
  name: "lookup",
  description: "Find a thing",
  input_schema: { type: "object", properties: { q: { type: "string" } } },
};
const withTools = (extra: Record<string, unknown> = {}) => ({
  ...demoInput,
  input: {
    messages: [{ role: "user", content: "Find it" }],
    tools: [lookup],
    ...extra,
  },
});
const history = [
  { role: "system", content: "Be brief" },
  { role: "user", content: "Find it" },
  {
    role: "assistant",
    content: "Looking",
    tool_calls: [{ id: "call_1", name: "lookup", arguments: '{"q":"a"}' }],
  },
  { role: "tool", tool_call_id: "call_1", content: "found" },
];
const accepts = (input: Record<string, unknown>) =>
  Invocation.safeParse({ ...demoInput, input }).success;

async function fixture(providers: Record<string, Provider>, service?: Service) {
  const store = new MemoryStore();
  const p = { ...principal, workspace: "tools-" + randomUUID() };
  await seed(store as never, p, service);
  const router = new Router(store, providers, digestKey);
  const key = randomUUID();
  const handler = createRouterHandler({
    router,
    store,
    keys: [{ key, principal: p }],
  });
  const post = (body: unknown, idempotency = randomUUID()) =>
    handler(
      new Request("http://localhost/v1/inferences", {
        method: "POST",
        headers: {
          authorization: "Bearer " + key,
          "idempotency-key": idempotency,
        },
        body: typeof body === "string" ? body : JSON.stringify(body),
      }),
    );
  return { store, p, router, post };
}
async function events(response: Response) {
  const text = await response.text();
  return text
    .split("\n\n")
    .filter(Boolean)
    .map((frame) => JSON.parse(frame.split("\ndata: ")[1]!));
}
const held = async (store: MemoryStore, p: typeof principal) =>
  (await store.balance(p)).map((row) => row.held + "/" + row.balance);
const sse = (items: unknown[]) =>
  new Response(
    items.map((item) => `data: ${JSON.stringify(item)}\n\n`).join(""),
  );
async function collect(provider: Provider, input: unknown) {
  const out: any[] = [];
  for await (const event of provider.invoke(
    Invocation.parse(input),
    demoService.offerings[0] as never,
    new AbortController().signal,
  ))
    out.push(event);
  return out;
}

test("a request without tools keeps its exact canonical form", () => {
  const parsed = Invocation.parse(demoInput);
  assert.equal("tools" in parsed.input, false);
  assert.equal("tool_choice" in parsed.input, false);
  assert.deepEqual(invocationNeeds(parsed), {
    tools: false,
    images: false,
    structured_output: false,
    temperature: false,
    top_p: false,
    stop: false,
    seed: false,
    reasoning_effort: false,
  });
  for (const key of ["sampling", "reasoning_effort", "tags", "response_format"])
    assert.equal(canonical(parsed).includes(key), false, key);
  assert.equal(canonical(parsed).includes("tool"), false);
});

test("the contract accepts tool history and image parts", () => {
  assert.ok(accepts({ messages: history, tools: [lookup] }));
  assert.ok(
    accepts({
      messages: [
        {
          role: "user",
          content: [
            { type: "text", text: "What is this?" },
            { type: "image", url: "https://e.example/a.png", detail: "low" },
            { type: "image", url: "data:image/png;base64,AAAA" },
          ],
        },
      ],
    }),
  );
});

test("the contract refuses malformed tool use", () => {
  const user = [{ role: "user", content: "Hi" }];
  const call = (id: string, args = "{}") => ({
    role: "assistant",
    tool_calls: [{ id, name: "lookup", arguments: args }],
  });
  const result = (id: string) => ({
    role: "tool",
    tool_call_id: id,
    content: "x",
  });
  const refused: Record<string, Record<string, unknown>> = {
    "duplicate tool": { messages: user, tools: [lookup, lookup] },
    "bad tool name": {
      messages: user,
      tools: [{ ...lookup, name: "a b" }],
    },
    "schema is not an object": {
      messages: user,
      tools: [{ ...lookup, input_schema: { type: "string" } }],
    },
    "choice without tools": { messages: user, tool_choice: "auto" },
    "choice of an undeclared tool": {
      messages: user,
      tools: [lookup],
      tool_choice: { name: "other" },
    },
    "result without a call": { messages: [...user, result("nope")] },
    "call left unanswered": { messages: [...user, call("a")] },
    "user message before the result": {
      messages: [...user, call("a"), ...user, result("a")],
    },
    "result sent twice": {
      messages: [...user, call("a"), result("a"), result("a")],
    },
    "reused call id": {
      messages: [...user, call("a"), result("a"), call("a"), result("a")],
    },
    "arguments are not a JSON object": {
      messages: [...user, call("a", "[1]"), result("a")],
    },
    "tool calls on a user message": {
      messages: [{ ...user[0], tool_calls: call("a").tool_calls }],
    },
    "image over http": {
      messages: [
        {
          role: "user",
          content: [{ type: "image", url: "http://e.example/a.png" }],
        },
      ],
    },
    "svg data url": {
      messages: [
        {
          role: "user",
          content: [{ type: "image", url: "data:image/svg+xml;base64,AAAA" }],
        },
      ],
    },
    "unknown part": {
      messages: [{ role: "user", content: [{ type: "audio", url: "x" }] }],
    },
  };
  for (const [name, input] of Object.entries(refused))
    assert.equal(accepts(input), false, name);
  let deep: Record<string, unknown> = { type: "object" };
  for (let i = 0; i < 40; i++)
    deep = { type: "object", properties: { a: deep } };
  assert.equal(
    accepts({ messages: user, tools: [{ ...lookup, input_schema: deep }] }),
    false,
    "schema nested too deeply",
  );
});

test("OpenAI: history, tools and streamed calls map to the Responses API", async () => {
  assert.deepEqual(
    toOpenAIInput(
      Invocation.parse({ ...demoInput, input: { messages: history } }).input
        .messages,
    ),
    [
      { role: "system", content: "Be brief" },
      { role: "user", content: "Find it" },
      { role: "assistant", content: "Looking" },
      {
        type: "function_call",
        call_id: "call_1",
        name: "lookup",
        arguments: '{"q":"a"}',
      },
      { type: "function_call_output", call_id: "call_1", output: "found" },
    ],
  );
  let sent: any;
  const provider = new OpenAIProvider(() => "test", (async (
    _url: string,
    init: RequestInit,
  ) => {
    sent = JSON.parse(String(init.body));
    return sse([
      { type: "response.output_text.delta", delta: "Hi" },
      {
        type: "response.output_item.added",
        output_index: 1,
        item: { type: "function_call", call_id: "call_9", name: "lookup" },
      },
      {
        type: "response.function_call_arguments.delta",
        output_index: 1,
        delta: '{"q"',
      },
      {
        type: "response.function_call_arguments.delta",
        output_index: 1,
        delta: ':"a"}',
      },
      {
        type: "response.function_call_arguments.done",
        output_index: 1,
        arguments: '{"q":"a"}',
      },
      {
        type: "response.output_item.added",
        output_index: 2,
        item: { type: "function_call", call_id: "call_10", name: "lookup" },
      },
      {
        type: "response.function_call_arguments.done",
        output_index: 2,
        arguments: "{}",
      },
      {
        type: "response.completed",
        response: { id: "r", usage: { input_tokens: 3, output_tokens: 2 } },
      },
    ]);
  }) as typeof fetch);
  const out = await collect(
    provider,
    withTools({ tool_choice: { name: "lookup" } }),
  );
  assert.deepEqual(sent.tools, [
    {
      type: "function",
      name: "lookup",
      description: "Find a thing",
      parameters: lookup.input_schema,
      strict: false,
    },
  ]);
  assert.deepEqual(sent.tool_choice, { type: "function", name: "lookup" });
  assert.deepEqual(
    out.filter((e) => e.type === "tool_call"),
    [
      {
        type: "tool_call",
        index: 0,
        id: "call_9",
        name: "lookup",
        arguments_delta: "",
      },
      { type: "tool_call", index: 0, arguments_delta: '{"q"' },
      { type: "tool_call", index: 0, arguments_delta: ':"a"}' },
      {
        type: "tool_call",
        index: 1,
        id: "call_10",
        name: "lookup",
        arguments_delta: "",
      },
      { type: "tool_call", index: 1, arguments_delta: "{}" },
    ],
  );
});

test("OpenAI: arguments for a call that never started are an upstream error", async () => {
  const provider = new OpenAIProvider(() => "test", (async () =>
    sse([
      {
        type: "response.function_call_arguments.delta",
        output_index: 4,
        delta: "{",
      },
    ])) as unknown as typeof fetch);
  await assert.rejects(
    () => collect(provider, withTools()),
    /invalid_upstream_event/,
  );
});

test("Anthropic: history, tools and streamed calls map to the Messages API", async () => {
  const parallel = [
    { role: "user", content: "Find both" },
    {
      role: "assistant",
      tool_calls: [
        { id: "a", name: "lookup", arguments: '{"q":"1"}' },
        { id: "b", name: "lookup", arguments: "{}" },
      ],
    },
    { role: "tool", tool_call_id: "a", content: "one" },
    { role: "tool", tool_call_id: "b", content: "bad", is_error: true },
  ];
  assert.deepEqual(
    toAnthropicMessages(
      Invocation.parse({ ...demoInput, input: { messages: parallel } }).input
        .messages,
    ),
    [
      { role: "user", content: "Find both" },
      {
        role: "assistant",
        content: [
          { type: "tool_use", id: "a", name: "lookup", input: { q: "1" } },
          { type: "tool_use", id: "b", name: "lookup", input: {} },
        ],
      },
      {
        role: "user",
        content: [
          { type: "tool_result", tool_use_id: "a", content: "one" },
          {
            type: "tool_result",
            tool_use_id: "b",
            content: "bad",
            is_error: true,
          },
        ],
      },
    ],
  );
  let sent: any;
  const provider = new AnthropicProvider(() => "test", (async (
    _url: string,
    init: RequestInit,
  ) => {
    sent = JSON.parse(String(init.body));
    return sse([
      {
        type: "message_start",
        message: { id: "m", usage: { input_tokens: 3, output_tokens: 0 } },
      },
      {
        type: "content_block_start",
        index: 1,
        content_block: {
          type: "tool_use",
          id: "toolu_1",
          name: "lookup",
          input: {},
        },
      },
      {
        type: "content_block_delta",
        index: 1,
        delta: { type: "input_json_delta", partial_json: '{"q":"a"}' },
      },
      { type: "content_block_stop", index: 1 },
      {
        type: "message_delta",
        delta: { stop_reason: "tool_use" },
        usage: { output_tokens: 4 },
      },
      { type: "message_stop" },
    ]);
  }) as typeof fetch);
  const out = await collect(provider, withTools({ tool_choice: "required" }));
  assert.deepEqual(sent.tools, [
    {
      name: "lookup",
      description: "Find a thing",
      input_schema: lookup.input_schema,
    },
  ]);
  assert.deepEqual(sent.tool_choice, { type: "any" });
  assert.deepEqual(
    out.filter((e) => e.type === "tool_call"),
    [
      {
        type: "tool_call",
        index: 0,
        id: "toolu_1",
        name: "lookup",
        arguments_delta: "",
      },
      { type: "tool_call", index: 0, arguments_delta: '{"q":"a"}' },
    ],
  );
  assert.equal(out.at(-1).completion, "succeeded");
});

test("a text request sends both providers the same body as before", async () => {
  const bodies: any[] = [];
  const capture = (async (_url: string, init: RequestInit) => {
    bodies.push(JSON.parse(String(init.body)));
    return new Response("");
  }) as typeof fetch;
  await assert.rejects(() =>
    collect(new OpenAIProvider(() => "k", capture), demoInput),
  );
  await assert.rejects(() =>
    collect(new AnthropicProvider(() => "k", capture), demoInput),
  );
  assert.deepEqual(bodies[0].input, demoInput.input.messages);
  assert.equal("tools" in bodies[0], false);
  assert.equal("tool_choice" in bodies[0], false);
  assert.deepEqual(bodies[1].messages, demoInput.input.messages);
  assert.equal("tools" in bodies[1], false);
});

test("streaming: a tool call arrives in order, settles once and reports usage", async () => {
  const provider = new MockToolProvider({ q: "a" });
  const { post, store, p } = await fixture({ mock: provider });
  const before = await store.balance(p);
  const list = await events(await post({ ...withTools(), stream: true }));
  assert.deepEqual(
    list.map((e) => e.type),
    [
      "request.started",
      "output.tool_call.delta",
      "output.tool_call.delta",
      "output.tool_call.completed",
      "request.completed",
    ],
  );
  const [first, second, done, end] = list.slice(1);
  assert.equal(first.name, "lookup");
  assert.equal(second.name, undefined);
  assert.equal(first.arguments_delta + second.arguments_delta, '{"q":"a"}');
  assert.equal(done.arguments, '{"q":"a"}');
  assert.equal(done.id, first.id);
  assert.equal(end.finish_reason, "tool_calls");
  assert.equal(end.billing_status, "settled");
  assert.deepEqual(end.usage, {
    status: "confirmed",
    meters: { input: 10, cached: 0, cache_write: 0, output: 5 },
  });
  assert.notDeepEqual(await store.balance(p), before);
  assert.ok((await store.balance(p)).every((row) => row.held === "0"));
});

test("non-streaming: the result carries the tool calls and the next turn answers in text", async () => {
  const { post } = await fixture({ mock: new MockToolProvider({ q: "a" }) });
  const first = (await (await post(withTools())).json()) as any;
  assert.equal(first.finish_reason, "tool_calls");
  assert.equal(first.output.tool_calls[0].name, "lookup");
  const call = first.output.tool_calls[0];
  const second = (await (
    await post({
      ...demoInput,
      input: {
        tools: [lookup],
        messages: [
          { role: "user", content: "Find it" },
          { role: "assistant", tool_calls: [call] },
          { role: "tool", tool_call_id: call.id, content: "found" },
        ],
      },
    })
  ).json()) as any;
  assert.equal(second.finish_reason, "stop");
  assert.equal(second.output.content, "The tool finished.");
  assert.equal("tool_calls" in second.output, false);
});

test("idempotency: a retry never runs or bills twice and a changed tool list conflicts", async () => {
  const provider = new MockToolProvider();
  const { post, store, p } = await fixture({ mock: provider });
  const key = randomUUID();
  assert.equal((await post(withTools(), key)).status, 200);
  const after = await store.balance(p);
  assert.equal((await post(withTools(), key)).status, 303);
  assert.equal(provider.calls, 1);
  assert.deepEqual(await store.balance(p), after);
  const changed = await post(
    withTools({ tools: [{ ...lookup, name: "other" }] }),
    key,
  );
  assert.equal(changed.status, 409);
  assert.equal(provider.calls, 1);
});

test("a provider without tool support is refused before anything is reserved", async () => {
  const provider = new MockProvider();
  const { post, store, p } = await fixture({ mock: provider });
  const before = await held(store, p);
  const image = {
    ...demoInput,
    input: {
      messages: [
        {
          role: "user",
          content: [{ type: "image", url: "https://e.example/a.png" }],
        },
      ],
    },
  };
  for (const body of [
    withTools(),
    image,
    { ...demoInput, input: { messages: history } },
  ]) {
    const response = await post(body);
    assert.equal(response.status, 422);
    assert.equal(
      ((await response.json()) as any).error.code,
      "unsupported_capability",
    );
  }
  assert.equal(provider.calls, 0);
  assert.deepEqual(await held(store, p), before);
});

test("routing skips an offering whose provider cannot carry tools", async () => {
  const base = demoService.offerings[0]!;
  const service = Service.parse({
    ...demoService,
    mode: "pool",
    offerings: [
      { ...base, id: "a-text", provider: "text", quality: 1 },
      { ...base, id: "b-tools", provider: "tools", quality: 0 },
    ],
  });
  const text = new MockProvider();
  const tools = new MockToolProvider();
  const { post } = await fixture({ text, tools }, service);
  assert.equal((await post(withTools())).status, 200);
  assert.equal(text.calls, 0);
  assert.equal(tools.calls, 1);
  // Plain text still reaches every offering.
  assert.equal((await post(demoInput)).status, 200);
  assert.equal(text.calls, 1);
});

test("a call cut off by the output limit is never reported as complete", async () => {
  const { post } = await fixture({
    mock: new MockToolProvider({ q: "abcdef" }, "truncated"),
  });
  const list = await events(await post({ ...withTools(), stream: true }));
  const types = list.map((e) => e.type);
  assert.equal(types.includes("output.tool_call.completed"), false);
  assert.equal(types.at(-1), "request.failed");
  assert.equal(list.at(-1).execution_status, "partial");
  assert.equal(list.at(-1).finish_reason, "length");
  assert.equal(list.at(-1).usage.status, "confirmed");
  const plain = (await (await post(withTools())).json()) as any;
  assert.equal("tool_calls" in plain.output, false);
});

test("a tool call nobody declared fails and keeps the hold for reconciliation", async () => {
  const rogue: Provider = {
    capabilities: { tools: true },
    async *invoke() {
      yield {
        type: "tool_call",
        index: 0,
        id: "x",
        name: "rogue",
        arguments_delta: "{}",
      };
      yield {
        type: "usage",
        usage: { input: 1, cached: 0, cache_write: 0, output: 1 },
      };
      yield { type: "done" };
    },
  };
  const { post, store, p } = await fixture({ mock: rogue });
  const list = await events(await post({ ...demoInput, stream: true }));
  assert.equal(list.at(-1).type, "request.failed");
  assert.notEqual(list.at(-1).execution_status, "succeeded");
  assert.ok((await store.balance(p)).some((row) => row.held !== "0"));
});

test("inference bodies may exceed the general limit, up to their own", async () => {
  const { post } = await fixture({ mock: new MockToolProvider() });
  const large = "x".repeat(250_000);
  const calls = ["a", "b"].map((id) => ({
    id,
    name: "lookup",
    arguments: "{}",
  }));
  const accepted = await post({
    ...demoInput,
    input: {
      tools: [lookup],
      messages: [
        { role: "user", content: "Go" },
        { role: "assistant", tool_calls: calls },
        { role: "tool", tool_call_id: "a", content: large },
        { role: "tool", tool_call_id: "b", content: large },
      ],
    },
  });
  assert.equal(accepted.status, 200);
  const refused = await post(JSON.stringify({ pad: "x".repeat(4_000_001) }));
  assert.equal(refused.status, 413);
});

test("sampling, reasoning effort and tags are needs a provider must declare", () => {
  const parsed = Invocation.parse({
    ...demoInput,
    sampling: { temperature: 0.2, stop: ["END"], seed: 1 },
    reasoning_effort: "low",
    tags: { team: "search" },
  });
  const needs = invocationNeeds(parsed);
  assert.deepEqual(
    Object.entries(needs)
      .filter(([, wanted]) => wanted)
      .map(([name]) => name),
    ["temperature", "stop", "seed", "reasoning_effort"],
  );
  // The shipped providers carry what their upstream takes and nothing they would drop.
  const openai = new OpenAIProvider(() => "k").capabilities;
  const anthropic = new AnthropicProvider(() => "k").capabilities;
  assert.deepEqual(
    [openai.temperature, openai.top_p, openai.reasoning_effort],
    [true, true, true],
  );
  assert.equal("stop" in openai || "seed" in openai, false);
  assert.deepEqual(
    [anthropic.temperature, anthropic.top_p, anthropic.stop],
    [true, true, true],
  );
  assert.equal("seed" in anthropic || "reasoning_effort" in anthropic, false);
});
