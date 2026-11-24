// Writes backends/rust/conformance/provider-gemini.v1.json: the request bodies and recorded streams the Gemini
// provider must reproduce, in the same shape as the `openai` and `anthropic` sections of tools.v1.json.
//
// Gemini has no TypeScript adapter, so unlike tools.v1.json this file is not generated from the reference. The
// expected values below are written by hand from Google's documented wire format; they are the oracle that the Rust
// and Go providers are both checked against.
//
//   node scripts/export-gemini-conformance.mjs
import { readFileSync, writeFileSync } from "node:fs";

const tools = JSON.parse(readFileSync(new URL("../backends/rust/conformance/tools.v1.json", import.meta.url), "utf8"));
const file = new URL("../backends/rust/conformance/provider-gemini.v1.json", import.meta.url);

const lookup = {
  name: "lookup",
  description: "Find a thing",
  input_schema: { type: "object", properties: { q: { type: "string" } }, required: ["q"] },
};
const other = { name: "other", input_schema: { type: "object" } };
const declared = { name: "lookup", description: "Find a thing", parametersJsonSchema: lookup.input_schema };
const declaredOther = { name: "other" };

const invocation = (input, maxOutput = 32) => ({
  service: "chat",
  customer_id: "c1",
  payer_mode: "developer",
  input,
  limits: { max_output_tokens: maxOutput, timeout_ms: 30000 },
  stream: false,
});
const user = (content) => ({ role: "user", content });

const requests = [];
const request = (name, input, body) => requests.push({ name, invocation: invocation(input), body });

request("plain text", { messages: [{ role: "system", content: "Be brief" }, user("Find it")] }, {
  contents: [{ role: "user", parts: [{ text: "Find it" }] }],
  systemInstruction: { parts: [{ text: "Be brief" }] },
  generationConfig: { maxOutputTokens: 32 },
});
request("several system messages", { messages: [{ role: "system", content: "A" }, user("hi"), { role: "system", content: "B" }] }, {
  contents: [{ role: "user", parts: [{ text: "hi" }] }],
  systemInstruction: { parts: [{ text: "A\nB" }] },
  generationConfig: { maxOutputTokens: 32 },
});
request("tools and auto", { messages: [user("Find it")], tools: [lookup, other], tool_choice: "auto" }, {
  contents: [{ role: "user", parts: [{ text: "Find it" }] }],
  generationConfig: { maxOutputTokens: 32 },
  tools: [{ functionDeclarations: [declared, declaredOther] }],
  toolConfig: { functionCallingConfig: { mode: "AUTO" } },
});
request("tools without a choice", { messages: [user("Find it")], tools: [lookup] }, {
  contents: [{ role: "user", parts: [{ text: "Find it" }] }],
  generationConfig: { maxOutputTokens: 32 },
  tools: [{ functionDeclarations: [declared] }],
});
request("tool choice none", { messages: [user("Find it")], tools: [lookup], tool_choice: "none" }, {
  contents: [{ role: "user", parts: [{ text: "Find it" }] }],
  generationConfig: { maxOutputTokens: 32 },
  tools: [{ functionDeclarations: [declared] }],
  toolConfig: { functionCallingConfig: { mode: "NONE" } },
});
request("tool choice required", { messages: [user("Find it")], tools: [lookup], tool_choice: "required" }, {
  contents: [{ role: "user", parts: [{ text: "Find it" }] }],
  generationConfig: { maxOutputTokens: 32 },
  tools: [{ functionDeclarations: [declared] }],
  toolConfig: { functionCallingConfig: { mode: "ANY" } },
});
request("tool choice by name", { messages: [user("Find it")], tools: [lookup, other], tool_choice: { name: "other" } }, {
  contents: [{ role: "user", parts: [{ text: "Find it" }] }],
  generationConfig: { maxOutputTokens: 32 },
  tools: [{ functionDeclarations: [declared, declaredOther] }],
  toolConfig: { functionCallingConfig: { mode: "ANY", allowedFunctionNames: ["other"] } },
});
request("history with text and calls", {
  messages: [
    { role: "system", content: "Be brief" },
    user("Find it"),
    { role: "assistant", content: "Looking", tool_calls: [{ id: "call_1", name: "lookup", arguments: '{"q":"a"}' }] },
    { role: "tool", tool_call_id: "call_1", content: "found" },
    { role: "assistant", content: "Found it." },
    user("Again"),
  ],
  tools: [lookup],
}, {
  contents: [
    { role: "user", parts: [{ text: "Find it" }] },
    { role: "model", parts: [{ text: "Looking" }, { functionCall: { name: "lookup", args: { q: "a" } } }] },
    { role: "user", parts: [{ functionResponse: { name: "lookup", response: { output: "found" } } }] },
    { role: "model", parts: [{ text: "Found it." }] },
    { role: "user", parts: [{ text: "Again" }] },
  ],
  systemInstruction: { parts: [{ text: "Be brief" }] },
  generationConfig: { maxOutputTokens: 32 },
  tools: [{ functionDeclarations: [declared] }],
});
request("parallel calls and an error result", {
  messages: [
    user("Find it"),
    { role: "assistant", tool_calls: [{ id: "a", name: "lookup", arguments: '{"q":"1"}' }, { id: "b", name: "other", arguments: '{ "q" : "2" }' }] },
    { role: "tool", tool_call_id: "a", content: "one" },
    { role: "tool", tool_call_id: "b", content: "bad", is_error: true },
    { role: "assistant", tool_calls: [{ id: "c", name: "lookup", arguments: "{}" }] },
    { role: "tool", tool_call_id: "c", content: "", is_error: false },
  ],
  tools: [lookup, other],
}, {
  contents: [
    { role: "user", parts: [{ text: "Find it" }] },
    { role: "model", parts: [{ functionCall: { name: "lookup", args: { q: "1" } } }, { functionCall: { name: "other", args: { q: "2" } } }] },
    {
      role: "user",
      parts: [
        { functionResponse: { name: "lookup", response: { output: "one" } } },
        { functionResponse: { name: "other", response: { error: "bad" } } },
      ],
    },
    { role: "model", parts: [{ functionCall: { name: "lookup", args: {} } }] },
    { role: "user", parts: [{ functionResponse: { name: "lookup", response: { output: "" } } }] },
  ],
  generationConfig: { maxOutputTokens: 32 },
  tools: [{ functionDeclarations: [declared, declaredOther] }],
});
request("a call signature goes back beside the call", {
  messages: [
    user("Find it"),
    {
      role: "assistant",
      tool_calls: [
        { id: "a", name: "lookup", arguments: '{"q":"1"}', signature: "c2lnLUE=" },
        { id: "b", name: "other", arguments: "{}" },
      ],
    },
    { role: "tool", tool_call_id: "a", content: "one" },
    { role: "tool", tool_call_id: "b", content: "two" },
  ],
  tools: [lookup, other],
}, {
  contents: [
    { role: "user", parts: [{ text: "Find it" }] },
    {
      role: "model",
      parts: [{ functionCall: { name: "lookup", args: { q: "1" } }, thoughtSignature: "c2lnLUE=" }, { functionCall: { name: "other", args: {} } }],
    },
    {
      role: "user",
      parts: [
        { functionResponse: { name: "lookup", response: { output: "one" } } },
        { functionResponse: { name: "other", response: { output: "two" } } },
      ],
    },
  ],
  generationConfig: { maxOutputTokens: 32 },
  tools: [{ functionDeclarations: [declared, declaredOther] }],
});
request("a tool with no parameters declares none", {
  messages: [user("Find it")],
  tools: [{ name: "ping", description: "Ping", input_schema: { type: "object", properties: {}, additionalProperties: false } }, lookup],
}, {
  contents: [{ role: "user", parts: [{ text: "Find it" }] }],
  generationConfig: { maxOutputTokens: 32 },
  tools: [{ functionDeclarations: [{ name: "ping", description: "Ping" }, declared] }],
});
request("inline images", {
  messages: [{ role: "user", content: [{ type: "text", text: "What is this?" }, { type: "image", url: "data:image/png;base64,AAAA", detail: "low" }] }],
}, {
  contents: [{ role: "user", parts: [{ text: "What is this?" }, { inlineData: { mimeType: "image/png", data: "AAAA" } }] }],
  generationConfig: { maxOutputTokens: 32 },
});

const streams = [];
const stream = (name, frames, events, error) => {
  const c = { name, invocation: invocation({ messages: [user("Find it")], tools: [lookup] }), frames, events };
  if (error) c.error = error;
  streams.push(c);
};
const text = (t) => ({ role: "model", parts: [{ text: t }] });
const usageEvent = (input, cached, output, id) => ({ type: "usage", usage: { input, cached, cache_write: 0, output }, upstream_id: id });
const done = (completion) => ({ type: "done", completion });

stream("text with cached input", [
  { candidates: [{ content: text("Hel") }], usageMetadata: { promptTokenCount: 7, totalTokenCount: 7 }, responseId: "r1" },
  { candidates: [{ content: text("lo"), finishReason: "STOP" }], usageMetadata: { promptTokenCount: 7, cachedContentTokenCount: 3, candidatesTokenCount: 2, totalTokenCount: 9 }, responseId: "r1" },
], [{ type: "delta", text: "Hel" }, { type: "delta", text: "lo" }, usageEvent(4, 3, 2, "r1"), done("succeeded")]);

stream("thinking is output and thoughts are not forwarded", [
  { candidates: [{ content: { role: "model", parts: [{ text: "pondering", thought: true }] } }], responseId: "r2" },
  {
    candidates: [{ content: text("Answer"), finishReason: "STOP" }],
    usageMetadata: { promptTokenCount: 100, cachedContentTokenCount: 40, candidatesTokenCount: 10, thoughtsTokenCount: 25, totalTokenCount: 135 },
    responseId: "r2",
  },
], [{ type: "delta", text: "Answer" }, usageEvent(60, 40, 35, "r2"), done("succeeded")]);

stream("one tool call", [
  {
    candidates: [{ content: { role: "model", parts: [{ text: "Checking. " }, { functionCall: { name: "lookup", args: { q: "x" } } }] }, finishReason: "STOP" }],
    usageMetadata: { promptTokenCount: 20, candidatesTokenCount: 5, totalTokenCount: 25 },
    responseId: "r3",
  },
], [
  { type: "delta", text: "Checking. " },
  { type: "tool_call", index: 0, id: "r3_0", name: "lookup", arguments_delta: '{"q":"x"}' },
  usageEvent(20, 0, 5, "r3"),
  done("succeeded"),
]);

stream("parallel calls, one with an id", [
  {
    candidates: [{ content: { role: "model", parts: [{ functionCall: { id: "c_a", name: "lookup", args: { q: "1" } } }, { functionCall: { name: "other" } }] }, finishReason: "STOP" }],
    usageMetadata: { promptTokenCount: 9, candidatesTokenCount: 4, totalTokenCount: 13 },
    responseId: "r4",
  },
], [
  { type: "tool_call", index: 0, id: "c_a", name: "lookup", arguments_delta: '{"q":"1"}' },
  { type: "tool_call", index: 1, id: "r4_1", name: "other", arguments_delta: "{}" },
  usageEvent(9, 0, 4, "r4"),
  done("succeeded"),
]);

stream("a signed call keeps its signature", [
  {
    candidates: [
      {
        content: {
          role: "model",
          parts: [
            { text: "pondering", thought: true },
            { functionCall: { name: "lookup", args: { q: "x" } }, thoughtSignature: "c2lnLUE=" },
            { functionCall: { name: "other", args: {} } },
          ],
        },
        finishReason: "STOP",
      },
    ],
    usageMetadata: { promptTokenCount: 20, candidatesTokenCount: 5, thoughtsTokenCount: 3, totalTokenCount: 28 },
    responseId: "r11",
  },
], [
  { type: "tool_call", index: 0, id: "r11_0", name: "lookup", signature: "c2lnLUE=", arguments_delta: '{"q":"x"}' },
  { type: "tool_call", index: 1, id: "r11_1", name: "other", arguments_delta: "{}" },
  usageEvent(20, 0, 8, "r11"),
  done("succeeded"),
]);

stream("a signature on text is not a call signature", [
  { candidates: [{ content: { role: "model", parts: [{ text: "Hi", thoughtSignature: "c2ln" }] }, finishReason: "STOP" }], usageMetadata: { promptTokenCount: 5, candidatesTokenCount: 1, totalTokenCount: 6 }, responseId: "r12" },
], [{ type: "delta", text: "Hi" }, usageEvent(5, 0, 1, "r12"), done("succeeded")]);

stream("an empty signature is no signature", [
  { candidates: [{ content: { role: "model", parts: [{ functionCall: { name: "lookup", args: {} }, thoughtSignature: "" }] }, finishReason: "STOP" }], usageMetadata: { promptTokenCount: 1, totalTokenCount: 1 }, responseId: "r13" },
], [{ type: "tool_call", index: 0, id: "r13_0", name: "lookup", arguments_delta: "{}" }, usageEvent(1, 0, 0, "r13"), done("succeeded")]);

stream("thoughts folded into the candidate count are not billed twice", [
  {
    candidates: [{ content: text("Answer"), finishReason: "STOP" }],
    usageMetadata: { promptTokenCount: 100, cachedContentTokenCount: 40, candidatesTokenCount: 35, thoughtsTokenCount: 25, totalTokenCount: 135 },
    responseId: "r14",
  },
], [{ type: "delta", text: "Answer" }, usageEvent(60, 40, 35, "r14"), done("succeeded")]);

stream("cut off by the output limit", [
  { candidates: [{ content: text("Hel"), finishReason: "MAX_TOKENS" }], usageMetadata: { promptTokenCount: 5, candidatesTokenCount: 1, totalTokenCount: 6 }, responseId: "r5" },
], [{ type: "delta", text: "Hel" }, usageEvent(5, 0, 1, "r5"), done("partial")]);

stream("usage in a chunk after the finish reason", [
  { candidates: [{ content: text("Hi"), finishReason: "STOP" }], usageMetadata: { promptTokenCount: 5, candidatesTokenCount: 1, totalTokenCount: 6 }, responseId: "r6" },
  { usageMetadata: { promptTokenCount: 5, candidatesTokenCount: 3, totalTokenCount: 8 }, responseId: "r6" },
], [{ type: "delta", text: "Hi" }, usageEvent(5, 0, 3, "r6"), done("succeeded")]);

stream("a blocked prompt", [
  { promptFeedback: { blockReason: "SAFETY" }, usageMetadata: { promptTokenCount: 7, totalTokenCount: 7 }, responseId: "r7" },
], [usageEvent(7, 0, 0, "r7"), done("filtered")]);

// The finish reasons that end a call by content policy are `filtered`; the rest that are not a natural stop stay
// `partial` (the output limit, a malformed call, anything the API adds later).
for (const [reason, completion] of [
  ["SAFETY", "filtered"], ["RECITATION", "filtered"], ["BLOCKLIST", "filtered"], ["PROHIBITED_CONTENT", "filtered"], ["SPII", "filtered"],
  ["MAX_TOKENS", "partial"], ["MALFORMED_FUNCTION_CALL", "partial"], ["OTHER", "partial"], ["SOMETHING_NEW", "partial"],
]) {
  stream(`finish reason ${reason}`, [
    { candidates: [{ content: text("x"), finishReason: reason }], usageMetadata: { promptTokenCount: 3, candidatesTokenCount: 1, totalTokenCount: 4 }, responseId: "rf" },
  ], [{ type: "delta", text: "x" }, usageEvent(3, 0, 1, "rf"), done(completion)]);
}

stream("a call without a name", [
  { candidates: [{ content: { role: "model", parts: [{ functionCall: { args: {} } }] }, finishReason: "STOP" }], usageMetadata: { promptTokenCount: 1 } },
], [], "invalid_upstream_event");
stream("arguments that are not an object", [
  { candidates: [{ content: { role: "model", parts: [{ functionCall: { name: "lookup", args: "q" } }] }, finishReason: "STOP" }], usageMetadata: { promptTokenCount: 1 } },
], [], "invalid_upstream_event");
stream("ends without a finish reason", [
  { candidates: [{ content: text("Hel") }], usageMetadata: { promptTokenCount: 5, totalTokenCount: 5 }, responseId: "r8" },
], [{ type: "delta", text: "Hel" }], "stream_interrupted");
stream("finishes without usage", [
  { candidates: [{ content: text("Hi"), finishReason: "STOP" }], responseId: "r9" },
], [{ type: "delta", text: "Hi" }], "usage_unknown");
stream("more cached than prompt", [
  { candidates: [{ content: text("Hi"), finishReason: "STOP" }], usageMetadata: { promptTokenCount: 5, cachedContentTokenCount: 6, candidatesTokenCount: 1 }, responseId: "r10" },
], [{ type: "delta", text: "Hi" }], "invalid_usage");
stream("an error frame", [
  { candidates: [{ content: text("Hel") }] },
  { error: { code: 500, message: "internal", status: "INTERNAL" } },
], [{ type: "delta", text: "Hel" }], "upstream_incomplete");

const out = {
  version: 1,
  description:
    "Provider cases for the Gemini generateContent adapter: exact upstream request bodies from an invocation, and the events produced from a recorded upstream stream. Written by scripts/export-gemini-conformance.mjs from Google's documented wire format; there is no TypeScript reference for this provider.",
  offering: { ...tools.offering, provider: "gemini" },
  gemini: { requests, streams },
};
writeFileSync(file, JSON.stringify(out, null, 1) + "\n");
console.log(`gemini: ${requests.length} requests, ${streams.length} streams`);
