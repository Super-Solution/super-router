// Writes conformance/tools.v1.json: language-neutral cases for tool calling and image input, generated from the
// TypeScript reference.
//
// - `invocation`: parsing untrusted JSON that carries tools, tool calls, tool results or image parts. Valid inputs
//   carry the parsed value and the canonical JSON that feeds the idempotency digest; invalid ones carry the error.
// - `openai` / `anthropic`: for each provider, `requests` pins the exact upstream request body built from a parsed
//   invocation, and `streams` pins the provider events produced from a recorded upstream stream (and the error code
//   when the stream must be refused).
//
// Every port runs these cases unchanged. Regenerate with `npx tsx scripts/export-tools-conformance.ts`.
import { writeFileSync, readFileSync } from "node:fs";
import { canonical } from "@supersolutions/super-router-core";
import {
  Invocation,
  ProviderError,
  type Offering,
} from "@supersolutions/super-router-contracts";
import { OpenAIProvider } from "@supersolutions/super-router-provider-openai";
import { AnthropicProvider } from "@supersolutions/super-router-provider-anthropic";

type Json = Record<string, any>;
const clone = <T>(x: T): T => JSON.parse(JSON.stringify(x));
const plane = JSON.parse(
  readFileSync(
    new URL("../conformance/dataplane.v1.json", import.meta.url),
    "utf8",
  ),
);
const baseInvocation: Json = plane.invocation;
const offering: Offering = {
  ...plane.services.chat.offerings[0],
  upstream_model: "upstream-model",
};

const lookup = {
  name: "lookup",
  description: "Find a thing",
  input_schema: {
    type: "object",
    properties: { q: { type: "string" } },
    required: ["q"],
  },
};
const user = { role: "user", content: "Find it" };
const call = (id: string, args = "{}", name = "lookup") => ({
  role: "assistant",
  tool_calls: [{ id, name, arguments: args }],
});
const result = (id: string, content = "found") => ({
  role: "tool",
  tool_call_id: id,
  content,
});

const invocations: Json[] = [];
/** A case is built from the base invocation; `input` replaces `input.input`. */
const invocation = (name: string, input: Json, edit?: (s: Json) => void) => {
  const value = clone(baseInvocation);
  value.input = clone(input);
  edit?.(value);
  const r = Invocation.safeParse(value);
  invocations.push({
    name,
    input: value,
    expect: r.success
      ? { ok: r.data, canonical: canonical(r.data) }
      : { error: "invalid_request" },
  });
};
/** A case whose request text matters byte for byte (number spellings JSON.stringify would rewrite): `raw` is the body. */
const rawInvocation = (name: string, raw: string) => {
  const r = Invocation.safeParse(JSON.parse(raw));
  invocations.push({
    name,
    raw,
    expect: r.success
      ? { ok: r.data, canonical: canonical(r.data) }
      : { error: "invalid_request" },
  });
};

// Accepted.
invocation("text only (no tool keys appear)", { messages: [user] });
invocation("tools declared", { messages: [user], tools: [lookup] });
invocation("tool without description", {
  messages: [user],
  tools: [{ name: "a", input_schema: { type: "object" } }],
});
for (const choice of ["auto", "none", "required", { name: "lookup" }])
  invocation(`tool_choice ${JSON.stringify(choice)}`, {
    messages: [user],
    tools: [lookup],
    tool_choice: choice,
  });
invocation("full history", {
  messages: [
    { role: "system", content: "Be brief" },
    user,
    {
      role: "assistant",
      content: "Looking",
      tool_calls: [{ id: "call_1", name: "lookup", arguments: '{"q":"a"}' }],
    },
    { role: "tool", tool_call_id: "call_1", content: "found" },
    { role: "assistant", content: "Done" },
    { role: "user", content: "Thanks" },
  ],
  tools: [lookup],
});
invocation("history without tools declared", {
  messages: [user, call("call_1"), result("call_1")],
});
invocation("parallel calls answered in another order", {
  messages: [
    user,
    {
      role: "assistant",
      tool_calls: [
        { id: "a", name: "lookup", arguments: '{"q":"1"}' },
        { id: "b", name: "lookup", arguments: "{}" },
      ],
    },
    { role: "tool", tool_call_id: "b", content: "two", is_error: true },
    { role: "tool", tool_call_id: "a", content: "one", is_error: false },
  ],
  tools: [lookup],
});
invocation("empty tool result", {
  messages: [user, call("a"), result("a", "")],
});
invocation("tool result 256000 code points", {
  messages: [user, call("a"), result("a", "é".repeat(256000))],
});
invocation("arguments with whitespace and nesting", {
  messages: [user, call("a", ' \n{ "q" : [1, {"x": null}] } '), result("a")],
});
invocation("several system messages", {
  messages: [
    { role: "system", content: "s".repeat(32000) },
    { role: "system", content: "more" },
    user,
  ],
});
invocation("64 tools", {
  messages: [user],
  tools: Array.from({ length: 64 }, (_, i) => ({
    name: "t" + i,
    input_schema: { type: "object" },
  })),
});
invocation("64 tool calls in one message", {
  messages: [
    user,
    {
      role: "assistant",
      tool_calls: Array.from({ length: 64 }, (_, i) => ({
        id: "c" + i,
        name: "lookup",
        arguments: "{}",
      })),
    },
    ...Array.from({ length: 64 }, (_, i) => result("c" + i)),
  ],
});
invocation("tool name and call id limits", {
  messages: [
    user,
    call("i".repeat(128), "{}", "N".repeat(64)),
    result("i".repeat(128)),
  ],
  tools: [{ name: "N".repeat(64), input_schema: { type: "object" } }],
});
invocation("schema keys sort by code point", {
  messages: [user],
  tools: [
    {
      name: "a",
      input_schema: {
        type: "object",
        properties: {
          userName: { type: "string" },
          user_id: { type: "string" },
          Zeta: { type: "string" },
          alpha: { type: "string" },
          $ref: { type: "string" },
          é: { type: "string" },
          "\u{1F600}": { type: "string" },
          Ａ: { type: "string" },
        },
      },
    },
  ],
});
rawInvocation(
  "schema numbers render like JavaScript",
  '{"service":"chat","customer_id":"c1","input":{"messages":[{"role":"user","content":"Find it"}],' +
    '"tools":[{"name":"a","input_schema":{"type":"object","properties":{"n":{"type":"number","minimum":1.0,' +
    '"maximum":1e21,"multipleOf":0.1,"exclusiveMinimum":-0,"default":12345678901234567890,' +
    '"examples":[1E-7,0.000001,123e-2,2e+3,1e300,5e-324,0.1e1,100,1.5e20,123456789012345680000]}}}}]},' +
    '"limits":{"max_output_tokens":32}}',
);
rawInvocation(
  "schema keys with escapes and duplicates",
  '{"service":"chat","customer_id":"c1","input":{"messages":[{"role":"user","content":"Find it"}],' +
    '"tools":[{"name":"a","input_schema":{"type":"object","\\u0041b":1,"d\\"q":2,"x":1,"x":2,' +
    '"t":"\\u00e9\\/\\u2028\\u0007"}}]},"limits":{"max_output_tokens":32}}',
);
invocation("schema may hold null, booleans and arrays", {
  messages: [user],
  tools: [
    {
      name: "a",
      input_schema: {
        type: "object",
        properties: { x: { enum: [null, true, false, "s", 1] } },
        additionalProperties: false,
        default: null,
      },
    },
  ],
});
let deep: Json = { type: "object" };
for (let i = 0; i < 31; i++) deep = { type: "object", p: deep };
invocation("schema nested 32 deep", {
  messages: [user],
  tools: [{ name: "a", input_schema: deep }],
});
invocation("image parts", {
  messages: [
    {
      role: "user",
      content: [
        { type: "text", text: "What is this?" },
        { type: "image", url: "https://e.example/a.png", detail: "low" },
        { type: "image", url: "data:image/png;base64,AAAA" },
        { type: "image", url: "data:image/jpeg;base64,AA==", detail: "high" },
        { type: "image", url: "data:image/gif;base64,AAA=" },
        { type: "image", url: "data:image/webp;base64,+/9a", detail: "auto" },
      ],
    },
  ],
});
invocation("32 parts", {
  messages: [
    {
      role: "user",
      content: Array.from({ length: 32 }, () => ({ type: "text", text: "x" })),
    },
  ],
});
invocation("https url 4096 units", {
  messages: [
    {
      role: "user",
      content: [
        { type: "image", url: "https://e.example/" + "a".repeat(4096 - 18) },
      ],
    },
  ],
});
invocation("tools with stream", { messages: [user], tools: [lookup] }, (v) => {
  v.stream = true;
});

// Refused.
const refused: Record<string, Json> = {
  "empty tools": { messages: [user], tools: [] },
  "65 tools": {
    messages: [user],
    tools: Array.from({ length: 65 }, (_, i) => ({
      name: "t" + i,
      input_schema: { type: "object" },
    })),
  },
  "duplicate tool": { messages: [user], tools: [lookup, lookup] },
  "bad tool name": { messages: [user], tools: [{ ...lookup, name: "a b" }] },
  "tool name 65 characters": {
    messages: [user],
    tools: [{ ...lookup, name: "a".repeat(65) }],
  },
  "empty description": {
    messages: [user],
    tools: [{ ...lookup, description: "" }],
  },
  "description 4097": {
    messages: [user],
    tools: [{ ...lookup, description: "d".repeat(4097) }],
  },
  "tool extra key": { messages: [user], tools: [{ ...lookup, strict: true }] },
  "tool without schema": {
    messages: [user],
    tools: [{ name: "a", description: "x" }],
  },
  "schema is not an object": {
    messages: [user],
    tools: [{ ...lookup, input_schema: { type: "string" } }],
  },
  "schema without type": {
    messages: [user],
    tools: [{ ...lookup, input_schema: { properties: {} } }],
  },
  "schema is an array": {
    messages: [user],
    tools: [{ ...lookup, input_schema: [{ type: "object" }] }],
  },
  "tool null description": {
    messages: [user],
    tools: [{ ...lookup, description: null }],
  },
  "choice without tools": { messages: [user], tool_choice: "auto" },
  "choice of an undeclared tool": {
    messages: [user],
    tools: [lookup],
    tool_choice: { name: "other" },
  },
  "unknown choice": { messages: [user], tools: [lookup], tool_choice: "any" },
  "choice extra key": {
    messages: [user],
    tools: [lookup],
    tool_choice: { name: "lookup", type: "tool" },
  },
  "result without a call": { messages: [user, result("nope")] },
  "call left unanswered": { messages: [user, call("a")] },
  "user message before the result": {
    messages: [user, call("a"), user, result("a")],
  },
  "result sent twice": {
    messages: [user, call("a"), result("a"), result("a")],
  },
  "reused call id": {
    messages: [user, call("a"), result("a"), call("a"), result("a")],
  },
  "arguments are an array": { messages: [user, call("a", "[1]"), result("a")] },
  "arguments are not JSON": { messages: [user, call("a", "{"), result("a")] },
  "arguments are empty": { messages: [user, call("a", ""), result("a")] },
  "arguments are null": { messages: [user, call("a", "null"), result("a")] },
  "arguments as an object": {
    messages: [
      user,
      {
        role: "assistant",
        tool_calls: [{ id: "a", name: "x", arguments: {} }],
      },
      result("a"),
    ],
  },
  "bad call id": {
    messages: [user, call("a b"), result("a b")],
  },
  "call id 129 characters": {
    messages: [user, call("i".repeat(129)), result("i".repeat(129))],
  },
  "empty tool calls": {
    messages: [user, { role: "assistant", tool_calls: [] }],
  },
  "65 tool calls": {
    messages: [
      user,
      {
        role: "assistant",
        tool_calls: Array.from({ length: 65 }, (_, i) => ({
          id: "c" + i,
          name: "lookup",
          arguments: "{}",
        })),
      },
      ...Array.from({ length: 65 }, (_, i) => result("c" + i)),
    ],
  },
  "assistant empty content with calls": {
    messages: [
      user,
      {
        role: "assistant",
        content: "",
        tool_calls: [{ id: "a", name: "x", arguments: "{}" }],
      },
      result("a"),
    ],
  },
  "tool calls on a user message": {
    messages: [{ ...user, tool_calls: call("a").tool_calls }],
  },
  "tool calls on a system message": {
    messages: [
      { role: "system", content: "s", tool_calls: call("a").tool_calls },
      user,
    ],
  },
  "tool result without content": {
    messages: [user, call("a"), { role: "tool", tool_call_id: "a" }],
  },
  "tool result 256001": {
    messages: [user, call("a"), result("a", "x".repeat(256001))],
  },
  "tool result with parts": {
    messages: [
      user,
      call("a"),
      {
        role: "tool",
        tool_call_id: "a",
        content: [{ type: "text", text: "x" }],
      },
    ],
  },
  "tool result is_error as string": {
    messages: [
      user,
      call("a"),
      { role: "tool", tool_call_id: "a", content: "x", is_error: "yes" },
    ],
  },
  "tool result extra key": {
    messages: [
      user,
      call("a"),
      { role: "tool", tool_call_id: "a", content: "x", name: "lookup" },
    ],
  },
  "task with tools": {
    task: { kind: "image", prompt: "a cat" },
    tools: [lookup],
  },
  "task with tool choice": {
    task: { kind: "image", prompt: "a cat" },
    tool_choice: "none",
  },
  "parts on an assistant message": {
    messages: [
      user,
      { role: "assistant", content: [{ type: "text", text: "x" }] },
    ],
  },
  "parts on a system message": {
    messages: [
      { role: "system", content: [{ type: "text", text: "x" }] },
      user,
    ],
  },
  "empty parts": { messages: [{ role: "user", content: [] }] },
  "33 parts": {
    messages: [
      {
        role: "user",
        content: Array.from({ length: 33 }, () => ({
          type: "text",
          text: "x",
        })),
      },
    ],
  },
  "empty text part": {
    messages: [{ role: "user", content: [{ type: "text", text: "" }] }],
  },
  "text part 32001": {
    messages: [
      { role: "user", content: [{ type: "text", text: "x".repeat(32001) }] },
    ],
  },
  "image over http": {
    messages: [
      {
        role: "user",
        content: [{ type: "image", url: "http://e.example/a.png" }],
      },
    ],
  },
  "https url with a space": {
    messages: [
      {
        role: "user",
        content: [{ type: "image", url: "https://e.example/a b" }],
      },
    ],
  },
  "https url with a no-break space": {
    messages: [
      {
        role: "user",
        content: [{ type: "image", url: "https://e.example/a b" }],
      },
    ],
  },
  "https url with an ideographic space": {
    messages: [
      {
        role: "user",
        content: [{ type: "image", url: "https://e.example/a　b" }],
      },
    ],
  },
  "https url 4097 units": {
    messages: [
      {
        role: "user",
        content: [
          { type: "image", url: "https://e.example/" + "a".repeat(4097 - 18) },
        ],
      },
    ],
  },
  "https url 4097 units from an astral character": {
    messages: [
      {
        role: "user",
        content: [
          {
            type: "image",
            url: "https://e.example/" + "a".repeat(4095 - 18) + "\u{1F600}",
          },
        ],
      },
    ],
  },
  "bare https scheme": {
    messages: [{ role: "user", content: [{ type: "image", url: "https://" }] }],
  },
  "svg data url": {
    messages: [
      {
        role: "user",
        content: [{ type: "image", url: "data:image/svg+xml;base64,AAAA" }],
      },
    ],
  },
  "data url without base64": {
    messages: [
      {
        role: "user",
        content: [{ type: "image", url: "data:image/png,AAAA" }],
      },
    ],
  },
  "data url bad padding": {
    messages: [
      {
        role: "user",
        content: [{ type: "image", url: "data:image/png;base64,AA===" }],
      },
    ],
  },
  "data url empty payload": {
    messages: [
      {
        role: "user",
        content: [{ type: "image", url: "data:image/png;base64," }],
      },
    ],
  },
  "data url with newline": {
    messages: [
      {
        role: "user",
        content: [{ type: "image", url: "data:image/png;base64,AA\nAA" }],
      },
    ],
  },
  "image unknown detail": {
    messages: [
      {
        role: "user",
        content: [
          { type: "image", url: "https://e.example/a.png", detail: "max" },
        ],
      },
    ],
  },
  "image extra key": {
    messages: [
      {
        role: "user",
        content: [
          { type: "image", url: "https://e.example/a.png", mime: "image/png" },
        ],
      },
    ],
  },
  "unknown part": {
    messages: [{ role: "user", content: [{ type: "audio", url: "x" }] }],
  },
  "role tool on its own": {
    messages: [{ role: "tool", content: "x" }],
  },
};
for (const [name, input] of Object.entries(refused)) invocation(name, input);
let tooDeep: Json = { type: "object" };
for (let i = 0; i < 32; i++) tooDeep = { type: "object", p: tooDeep };
invocation("schema nested 33 deep", {
  messages: [user],
  tools: [{ name: "a", input_schema: tooDeep }],
});
let deepArray: Json = { type: "object" };
let leaf: unknown = 1;
for (let i = 0; i < 32; i++) leaf = [leaf];
deepArray = { type: "object", enum: leaf };
invocation("schema array nested 33 deep", {
  messages: [user],
  tools: [{ name: "a", input_schema: deepArray }],
});

for (const c of invocations) {
  if (c.name.startsWith("text only") && c.expect.canonical.includes("tool"))
    throw new Error("a request without tools must not mention them");
}

// Provider mapping. `requests` records the body each adapter sends; `streams` records the events it yields.
type Body = Json;
const capture = async (
  make: (transport: typeof fetch) => {
    invoke: (...a: any[]) => AsyncIterable<any>;
  },
  input: Json,
  frames: unknown[] | null,
) => {
  let body: Body | undefined;
  const transport = (async (_url: string, init: RequestInit) => {
    body = JSON.parse(String(init.body));
    return new Response(
      (frames ?? [])
        .map(
          (f) => `data: ${typeof f === "string" ? f : JSON.stringify(f)}\n\n`,
        )
        .join(""),
    );
  }) as typeof fetch;
  const events: unknown[] = [];
  let error: string | undefined;
  try {
    for await (const e of make(transport).invoke(
      Invocation.parse(input),
      offering,
      new AbortController().signal,
    ))
      events.push(e);
  } catch (e) {
    if (!(e instanceof ProviderError)) throw e;
    error = e.code;
  }
  return { body: body!, events, error };
};
const openai = (t: typeof fetch) => new OpenAIProvider(() => "test-key", t);
const anthropic = (t: typeof fetch) =>
  new AnthropicProvider(() => "test-key", t);
const inv = (input: Json, edit?: (v: Json) => void) => {
  const v = clone(baseInvocation);
  v.input = input;
  edit?.(v);
  return Invocation.parse(v);
};

/** Cases only the providers that carry the control are asked: the others refuse it before reserving (capabilities). */
const onlyFor: Record<string, string[]> = {
  "sampling stop sequences": ["anthropic"],
  "reasoning effort": ["openai"],
};
const requestInputs: Record<string, Json> = {
  "plain text": inv({
    messages: [{ role: "system", content: "Be brief" }, user],
  }),
  "several system messages": inv({
    messages: [
      { role: "system", content: "One" },
      { role: "system", content: "Two" },
      user,
    ],
  }),
  "tools and auto": inv({
    messages: [user],
    tools: [lookup],
    tool_choice: "auto",
  }),
  "tool choice none": inv({
    messages: [user],
    tools: [lookup],
    tool_choice: "none",
  }),
  "tool choice required": inv({
    messages: [user],
    tools: [lookup],
    tool_choice: "required",
  }),
  "tool choice by name": inv({
    messages: [user],
    tools: [lookup, { name: "other", input_schema: { type: "object" } }],
    tool_choice: { name: "other" },
  }),
  "history with text and calls": inv({
    messages: [
      { role: "system", content: "Be brief" },
      user,
      {
        role: "assistant",
        content: "Looking",
        tool_calls: [{ id: "call_1", name: "lookup", arguments: '{"q":"a"}' }],
      },
      { role: "tool", tool_call_id: "call_1", content: "found" },
      { role: "assistant", content: "Found it." },
      { role: "user", content: "Again" },
    ],
    tools: [lookup],
  }),
  "parallel calls and an error result": inv({
    messages: [
      user,
      {
        role: "assistant",
        tool_calls: [
          { id: "a", name: "lookup", arguments: '{"q":"1"}' },
          { id: "b", name: "lookup", arguments: '{ "q" : "2" }' },
        ],
      },
      { role: "tool", tool_call_id: "a", content: "one" },
      { role: "tool", tool_call_id: "b", content: "bad", is_error: true },
      {
        role: "assistant",
        tool_calls: [{ id: "c", name: "lookup", arguments: "{}" }],
      },
      { role: "tool", tool_call_id: "c", content: "", is_error: false },
    ],
    tools: [lookup],
  }),
  "sampling temperature and top_p": inv({ messages: [user] }, (v) => {
    v.sampling = { temperature: 0.3, top_p: 0.8 };
  }),
  "sampling stop sequences": inv({ messages: [user] }, (v) => {
    v.sampling = { stop: ["END", "\n\n"] };
  }),
  "reasoning effort": inv({ messages: [user] }, (v) => {
    v.reasoning_effort = "medium";
  }),
  "structured output": inv({ messages: [user] }, (v) => {
    v.response_format = {
      type: "json_schema",
      json_schema: {
        name: "weather",
        description: "A reading",
        schema: {
          type: "object",
          properties: { city: { type: "string" }, temp: { type: "number" } },
          required: ["city", "temp"],
          additionalProperties: false,
        },
        strict: true,
      },
    };
  }),
  "structured output without description": inv({ messages: [user] }, (v) => {
    v.response_format = {
      type: "json_schema",
      json_schema: {
        name: "flag",
        schema: {
          type: "object",
          properties: { ok: { type: "boolean" } },
          required: ["ok"],
          additionalProperties: false,
        },
      },
    };
  }),
  "automatic caching": inv({ messages: [user] }, (v) => {
    v.cache = { mode: "auto" };
  }),
  "caching none sends nothing": inv({ messages: [user] }, (v) => {
    v.cache = { mode: "none" };
  }),
  "long output limit": inv({ messages: [user] }, (v) => {
    v.limits.max_output_tokens = 128000;
  }),
  images: inv({
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
};

const streamCases: Record<
  string,
  { openai?: unknown[]; anthropic?: unknown[]; input?: Json }
> = {
  "text only": {
    openai: [
      { type: "response.output_text.delta", delta: "Hel" },
      { type: "response.output_text.delta", delta: "lo" },
      {
        type: "response.completed",
        response: {
          id: "resp_1",
          usage: {
            input_tokens: 10,
            output_tokens: 2,
            input_tokens_details: { cached_tokens: 3 },
          },
        },
      },
    ],
    anthropic: [
      {
        type: "message_start",
        message: {
          id: "msg_1",
          usage: {
            input_tokens: 7,
            output_tokens: 1,
            cache_read_input_tokens: 3,
          },
        },
      },
      {
        type: "content_block_start",
        index: 0,
        content_block: { type: "text", text: "" },
      },
      {
        type: "content_block_delta",
        index: 0,
        delta: { type: "text_delta", text: "Hel" },
      },
      {
        type: "content_block_delta",
        index: 0,
        delta: { type: "text_delta", text: "lo" },
      },
      { type: "content_block_stop", index: 0 },
      {
        type: "message_delta",
        delta: { stop_reason: "end_turn" },
        usage: { output_tokens: 2 },
      },
      { type: "message_stop" },
    ],
  },
  "text then streamed calls": {
    openai: [
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
        item: { type: "function_call", call_id: "call_10", name: "other" },
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
    ],
    anthropic: [
      {
        type: "message_start",
        message: { id: "m", usage: { input_tokens: 3, output_tokens: 0 } },
      },
      {
        type: "content_block_start",
        index: 0,
        content_block: { type: "text", text: "" },
      },
      {
        type: "content_block_delta",
        index: 0,
        delta: { type: "text_delta", text: "Hi" },
      },
      { type: "content_block_stop", index: 0 },
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
        delta: { type: "input_json_delta", partial_json: '{"q"' },
      },
      {
        type: "content_block_delta",
        index: 1,
        delta: { type: "input_json_delta", partial_json: ':"a"}' },
      },
      { type: "content_block_stop", index: 1 },
      {
        type: "content_block_start",
        index: 2,
        content_block: {
          type: "tool_use",
          id: "toolu_2",
          name: "other",
          input: {},
        },
      },
      { type: "content_block_stop", index: 2 },
      {
        type: "message_delta",
        delta: { stop_reason: "tool_use" },
        usage: { output_tokens: 4 },
      },
      { type: "message_stop" },
    ],
  },
  "arguments only at the end": {
    openai: [
      {
        type: "response.output_item.added",
        output_index: 0,
        item: { type: "function_call", call_id: "c", name: "lookup" },
      },
      {
        type: "response.function_call_arguments.done",
        output_index: 0,
        arguments: '{"q":"z"}',
      },
      {
        type: "response.completed",
        response: { id: "r", usage: { input_tokens: 1, output_tokens: 1 } },
      },
    ],
    anthropic: [
      {
        type: "message_start",
        message: { id: "m", usage: { input_tokens: 1, output_tokens: 0 } },
      },
      {
        type: "content_block_start",
        index: 0,
        content_block: {
          type: "tool_use",
          id: "c",
          name: "lookup",
          input: { q: "z", n: [1, 2] },
        },
      },
      { type: "content_block_stop", index: 0 },
      {
        type: "message_delta",
        delta: { stop_reason: "tool_use" },
        usage: { output_tokens: 1 },
      },
      { type: "message_stop" },
    ],
  },
  "cut off by the output limit": {
    openai: [
      {
        type: "response.output_item.added",
        output_index: 0,
        item: { type: "function_call", call_id: "c", name: "lookup" },
      },
      {
        type: "response.function_call_arguments.delta",
        output_index: 0,
        delta: '{"q":',
      },
      {
        type: "response.incomplete",
        response: { id: "r", usage: { input_tokens: 1, output_tokens: 32 } },
      },
    ],
    anthropic: [
      {
        type: "message_start",
        message: { id: "m", usage: { input_tokens: 1, output_tokens: 0 } },
      },
      {
        type: "content_block_start",
        index: 0,
        content_block: { type: "tool_use", id: "c", name: "lookup", input: {} },
      },
      {
        type: "content_block_delta",
        index: 0,
        delta: { type: "input_json_delta", partial_json: '{"q":' },
      },
      {
        type: "message_delta",
        delta: { stop_reason: "max_tokens" },
        usage: { output_tokens: 32 },
      },
      { type: "message_stop" },
    ],
  },
  "arguments for a call that never started": {
    openai: [
      {
        type: "response.function_call_arguments.delta",
        output_index: 4,
        delta: "{",
      },
    ],
    anthropic: [
      {
        type: "message_start",
        message: { id: "m", usage: { input_tokens: 1, output_tokens: 0 } },
      },
      {
        type: "content_block_delta",
        index: 4,
        delta: { type: "input_json_delta", partial_json: "{" },
      },
    ],
  },
  "a call without an id": {
    openai: [
      {
        type: "response.output_item.added",
        output_index: 0,
        item: { type: "function_call", name: "lookup" },
      },
    ],
    anthropic: [
      {
        type: "message_start",
        message: { id: "m", usage: { input_tokens: 1, output_tokens: 0 } },
      },
      {
        type: "content_block_start",
        index: 0,
        content_block: { type: "tool_use", name: "lookup", input: {} },
      },
    ],
  },
  "a call without a name": {
    openai: [
      {
        type: "response.output_item.added",
        output_index: 0,
        item: { type: "function_call", call_id: "c" },
      },
    ],
    anthropic: [
      {
        type: "message_start",
        message: { id: "m", usage: { input_tokens: 1, output_tokens: 0 } },
      },
      {
        type: "content_block_start",
        index: 0,
        content_block: { type: "tool_use", id: "c", input: {} },
      },
    ],
  },
  "a position started twice": {
    openai: [
      {
        type: "response.output_item.added",
        output_index: 0,
        item: { type: "function_call", call_id: "a", name: "lookup" },
      },
      {
        type: "response.output_item.added",
        output_index: 0,
        item: { type: "function_call", call_id: "b", name: "lookup" },
      },
    ],
    anthropic: [
      {
        type: "message_start",
        message: { id: "m", usage: { input_tokens: 1, output_tokens: 0 } },
      },
      {
        type: "content_block_start",
        index: 0,
        content_block: { type: "tool_use", id: "a", name: "lookup", input: {} },
      },
      {
        type: "content_block_start",
        index: 0,
        content_block: { type: "tool_use", id: "b", name: "lookup", input: {} },
      },
    ],
  },
  "arguments that are not a string": {
    openai: [
      {
        type: "response.output_item.added",
        output_index: 0,
        item: { type: "function_call", call_id: "a", name: "lookup" },
      },
      {
        type: "response.function_call_arguments.delta",
        output_index: 0,
        delta: 7,
      },
    ],
    anthropic: [
      {
        type: "message_start",
        message: { id: "m", usage: { input_tokens: 1, output_tokens: 0 } },
      },
      {
        type: "content_block_start",
        index: 0,
        content_block: { type: "tool_use", id: "a", name: "lookup", input: {} },
      },
      {
        type: "content_block_delta",
        index: 0,
        delta: { type: "input_json_delta", partial_json: 7 },
      },
    ],
  },
  "stream ends inside a call": {
    openai: [
      {
        type: "response.output_item.added",
        output_index: 0,
        item: { type: "function_call", call_id: "a", name: "lookup" },
      },
      {
        type: "response.function_call_arguments.delta",
        output_index: 0,
        delta: "{",
      },
    ],
    anthropic: [
      {
        type: "message_start",
        message: { id: "m", usage: { input_tokens: 1, output_tokens: 0 } },
      },
      {
        type: "content_block_start",
        index: 0,
        content_block: { type: "tool_use", id: "a", name: "lookup", input: {} },
      },
      {
        type: "content_block_delta",
        index: 0,
        delta: { type: "input_json_delta", partial_json: "{" },
      },
    ],
  },
};

const toolsInput = inv({
  messages: [user],
  tools: [lookup, { name: "other", input_schema: { type: "object" } }],
});
const providers = { openai, anthropic } as const;
const out: Json = {
  version: 1,
  description:
    "Generated by scripts/export-tools-conformance.ts from the TypeScript reference. Do not edit by hand.",
  offering,
  invocation: invocations,
};
for (const [name, make] of Object.entries(providers)) {
  const requests: Json[] = [];
  for (const [caseName, input] of Object.entries(requestInputs)) {
    if (onlyFor[caseName] && !onlyFor[caseName].includes(name)) continue;
    const r = await capture(make, input, []);
    requests.push({ name: caseName, invocation: input, body: r.body });
  }
  const streams: Json[] = [];
  for (const [caseName, c] of Object.entries(streamCases)) {
    const frames = c[name as "openai" | "anthropic"];
    if (!frames) continue;
    const input = c.input ?? toolsInput;
    const r = await capture(make, input, frames);
    streams.push({
      name: caseName,
      invocation: input,
      frames,
      events: r.events,
      ...(r.error ? { error: r.error } : {}),
    });
  }
  out[name] = { requests, streams };
}
// The reference's file and the Rust copy (which Rust and Go read) are the same bytes.
for (const path of [
  "../conformance/tools.v1.json",
  "../backends/rust/conformance/tools.v1.json",
])
  writeFileSync(
    new URL(path, import.meta.url),
    JSON.stringify(out, null, 1) + "\n",
  );
console.log(
  `tools.v1.json: ${invocations.length} invocations, ${Object.keys(requestInputs).length} requests and ${Object.keys(streamCases).length} streams per provider`,
);
