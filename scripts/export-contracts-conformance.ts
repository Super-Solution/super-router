// Writes conformance/contracts.v1.json: language-neutral cases for parsing untrusted JSON into a service or an
// invocation. Valid inputs carry the parsed result (defaults filled) and, for invocations, the canonical JSON
// that feeds the idempotency digest; invalid ones carry the error code. The Go and Python parsers must agree
// on every case, so a service published through one backend behaves the same in another.
import { readFileSync, writeFileSync } from "node:fs";
import { canonical } from "@supersolutions/super-router-core";
import { Invocation, Service } from "@supersolutions/super-router-contracts";

type Json = Record<string, any>;
const plane = JSON.parse(
  readFileSync(
    new URL("../conformance/dataplane.v1.json", import.meta.url),
    "utf8",
  ),
);
const baseService: Json = plane.services.chat;
const baseInvocation: Json = plane.invocation;
const clone = <T>(x: T): T => JSON.parse(JSON.stringify(x));

const services: Json[] = [];
const invocations: Json[] = [];
const service = (name: string, edit?: (s: Json) => void) => {
  const input = clone(baseService);
  edit?.(input);
  const r = Service.safeParse(input);
  services.push({
    name,
    input,
    expect: r.success ? { ok: r.data } : { error: "invalid_request" },
  });
};
const invocation = (name: string, edit?: (s: Json) => void) => {
  const input = clone(baseInvocation);
  edit?.(input);
  const r = Invocation.safeParse(input);
  invocations.push({
    name,
    input,
    expect: r.success
      ? { ok: r.data, canonical: canonical(r.data) }
      : { error: "invalid_request" },
  });
};
const o = (s: Json) => s.offerings[0];

service("base");
service("defaults omitted", (s) => {
  delete s.required_labels;
  delete s.unknown_score;
  delete s.price.per;
  delete o(s).labels;
  delete o(s).price.per;
});
service("pool mode", (s) => (s.mode = "pool"));
service("specific mode with model", (s) => {
  s.mode = "specific";
  s.model = "mock-model";
});
service("platform price and subscription", (s) => {
  s.platform_price = clone(s.price);
  s.requires_subscription = true;
});
service("task service with media price", (s) => {
  s.task_kind = "image";
  o(s).task_kind = "image";
  o(s).max_usage = { images: 4 };
  o(s).price.media = { images: { atoms: "100", per: 1 } };
  s.price.media = { images: { atoms: "200", per: 1 } };
});
service("weights sum within 1e-9", (s) => {
  s.weights = { cost: 0.5, latency: 0, reliability: 0, quality: 0.5000000001 };
});
service("optional scores", (s) => {
  Object.assign(o(s), { latency_ms: 120.5, reliability: 0.99, quality: 0 });
});
service("max safe integer version", (s) => (s.version = 9007199254740991));
service("long output service and offering", (s) => {
  s.max_output_tokens = 131072;
  o(s).max_output_tokens = 131072;
});
const serviceMutations: Record<string, (s: Json) => unknown> = {
  "unknown top-level key": (s: Json) => (s.extra = 1),
  "missing id": (s: Json) => delete s.id,
  "id with space": (s: Json) => (s.id = "a b"),
  "id 121 characters": (s: Json) => (s.id = "a".repeat(121)),
  "empty id": (s: Json) => (s.id = ""),
  "version zero": (s: Json) => (s.version = 0),
  "version fractional": (s: Json) => (s.version = 1.5),
  "version above safe": (s: Json) => (s.version = 9007199254740992),
  "unknown mode": (s: Json) => (s.mode = "auto"),
  "specific without model": (s: Json) => {
    s.mode = "specific";
    delete s.model;
  },
  "tier without tier": (s: Json) => {
    s.mode = "tier";
    delete s.tier;
  },
  "no offerings": (s: Json) => (s.offerings = []),
  "duplicate offering": (s: Json) => s.offerings.push(clone(o(s))),
  "mixed supply currency": (s: Json) => {
    const b = clone(o(s));
    b.id = "b";
    b.price.asset = "EUR";
    s.offerings.push(b);
  },
  "no regions": (s: Json) => (s.regions = []),
  "weights sum 0.9": (s: Json) => (s.weights.quality = -0.1 + 0),
  "weights sum above one": (s: Json) => (s.weights.latency = 0.1),
  "negative weight": (s: Json) => {
    s.weights.cost = 1.5;
    s.weights.latency = -0.5;
  },
  "missing weight": (s: Json) => delete s.weights.quality,
  "attempts zero": (s: Json) => (s.max_attempts = 0),
  "attempts six": (s: Json) => (s.max_attempts = 6),
  "provider cost leading zero": (s: Json) => (s.max_provider_cost = "01"),
  "provider cost negative": (s: Json) => (s.max_provider_cost = "-1"),
  "provider cost 31 digits": (s: Json) =>
    (s.max_provider_cost = "1".repeat(31)),
  "cost scale zero": (s: Json) => (s.cost_scale = "0"),
  "cost scale as number": (s: Json) => (s.cost_scale = 10000),
  "latency scale zero": (s: Json) => (s.latency_scale_ms = 0),
  "unknown score above one": (s: Json) => (s.unknown_score = 1.5),
  "unknown score null": (s: Json) => (s.unknown_score = null),
  "max output tokens zero": (s: Json) => (s.max_output_tokens = 0),
  "max output tokens 131073": (s: Json) => (s.max_output_tokens = 131073),
  "task kind unknown": (s: Json) => (s.task_kind = "pdf"),
  "price missing asset": (s: Json) => delete s.price.asset,
  "price per zero": (s: Json) => (s.price.per = 0),
  "price per too large": (s: Json) => (s.price.per = 1_000_000_001),
  "price input negative": (s: Json) => (s.price.input = "-1"),
  "price extra key": (s: Json) => (s.price.currency = "x"),
  "media meter unknown": (s: Json) =>
    (s.price.media = { pixels: { atoms: "1", per: 1 } }),
  "media rate per zero": (s: Json) =>
    (s.price.media = { images: { atoms: "1", per: 0 } }),
  "offering extra key": (s: Json) => (o(s).note = "x"),
  "offering source unknown": (s: Json) => (o(s).source = "other"),
  "offering input tokens zero": (s: Json) => (o(s).max_input_tokens = 0),
  "offering input tokens too large": (s: Json) =>
    (o(s).max_input_tokens = 1_000_001),
  "offering output tokens 131073": (s: Json) =>
    (o(s).max_output_tokens = 131073),
  "offering reliability above one": (s: Json) => (o(s).reliability = 1.1),
  "offering latency negative": (s: Json) => (o(s).latency_ms = -1),
  "offering enabled missing": (s: Json) => delete o(s).enabled,
  "offering label invalid": (s: Json) => (o(s).labels = ["a b"]),
  "offering max usage unknown meter": (s: Json) =>
    (o(s).max_usage = { pixels: 1 }),
  "offering max usage negative": (s: Json) => (o(s).max_usage = { images: -1 }),
};
for (const [name, edit] of Object.entries(serviceMutations))
  service(name, edit);

invocation("base");
invocation("defaults omitted", (i) => {
  delete i.payer_mode;
  delete i.stream;
  delete i.input.messages;
  delete i.limits.max_output_tokens;
  delete i.limits.timeout_ms;
});
invocation("service version", (i) => (i.service_version = 3));
invocation("max charge", (i) => {
  i.limits.max_charge = { asset: "credit:demo", atoms: "500", scale: 9 };
});
invocation("task", (i) => {
  i.input = { task: { kind: "image", prompt: "a cat" } };
  i.limits.max_usage = { images: 2 };
});
invocation("stream flag", (i) => (i.stream = true));
invocation("long output and long timeout", (i) => {
  i.limits.max_output_tokens = 131072;
  i.limits.timeout_ms = 600000;
});
invocation("sampling", (i) => {
  i.sampling = {
    temperature: 0.2,
    top_p: 0.9,
    stop: ["END", "\n\n"],
    seed: -7,
  };
});
invocation("sampling single field", (i) => (i.sampling = { top_p: 1 }));
invocation("sampling temperature bounds", (i) => {
  i.sampling = { temperature: 0 };
});
invocation("reasoning effort", (i) => (i.reasoning_effort = "high"));
invocation("cache auto", (i) => (i.cache = { mode: "auto" }));
invocation("cache none", (i) => (i.cache = { mode: "none" }));
invocation("tags", (i) => {
  i.tags = {
    team: "search",
    "env:stage": "ünï 🙂 ok",
    "a.b_c-d": "x".repeat(256),
  };
});
invocation("structured output", (i) => {
  i.response_format = {
    type: "json_schema",
    json_schema: {
      name: "answer",
      strict: true,
      schema: {
        type: "object",
        properties: { answer: { type: ["string", "null"], default: null } },
        required: ["answer"],
        additionalProperties: false,
      },
    },
  };
});
invocation("structured strict omitted", (i) => {
  i.response_format = {
    type: "json_schema",
    json_schema: {
      name: "answer",
      schema: {
        type: "object",
        properties: { answer: { type: ["string", "null"], default: null } },
        required: ["answer"],
        additionalProperties: false,
      },
    },
  };
});
invocation("unicode content", (i) => {
  i.input.messages = [{ role: "user", content: 'héllo   "q" \\ \n 🙂 <&>' }];
});
invocation("100 messages", (i) => {
  i.input.messages = Array.from({ length: 100 }, () => ({
    role: "user",
    content: "x",
  }));
});
invocation("content 32000 UTF-16 units", (i) => {
  i.input.messages = [{ role: "user", content: "a".repeat(32000) }];
});
invocation("content 16000 emoji (32000 units)", (i) => {
  i.input.messages = [{ role: "user", content: "😀".repeat(16000) }];
});
const schemaOf = (): Json => ({
  type: "object",
  properties: { answer: { type: ["string", "null"], default: null } },
  required: ["answer"],
  additionalProperties: false,
});
const format = (edit?: (j: Json) => void) => {
  const json_schema: Json = {
    name: "answer",
    strict: true,
    schema: schemaOf(),
  };
  edit?.(json_schema);
  return { type: "json_schema", json_schema };
};
const invocationMutations: Record<string, (s: Json) => unknown> = {
  "structured schema invalid": (i: Json) =>
    (i.response_format = format((j) => (j.schema = []))),
  "structured strict false": (i: Json) =>
    (i.response_format = format((j) => (j.strict = false))),
  "structured unknown key": (i: Json) => {
    i.response_format = format();
    i.response_format.extra = true;
  },
  "structured on task": (i: Json) => {
    i.input = { task: { kind: "image", prompt: "p" } };
    i.response_format = format();
  },
  "structured name with space": (i: Json) =>
    (i.response_format = format((j) => (j.name = "a b"))),
  "structured description 1025 characters": (i: Json) =>
    (i.response_format = format((j) => (j.description = "d".repeat(1025)))),
  "structured type text": (i: Json) => {
    i.response_format = format();
    i.response_format.type = "text";
  },
  "structured null": (i: Json) => (i.response_format = null),
  "unknown key": (i: Json) => (i.extra = 1),
  "payer mode platform": (i: Json) => (i.payer_mode = "platform"),
  "missing service": (i: Json) => delete i.service,
  "empty customer": (i: Json) => (i.customer_id = ""),
  "service version zero": (i: Json) => (i.service_version = 0),
  "input missing": (i: Json) => delete i.input,
  "limits missing": (i: Json) => delete i.limits,
  "input null": (i: Json) => (i.input = null),
  "empty content": (i: Json) => (i.input.messages[0].content = ""),
  "content 32001 units": (i: Json) =>
    (i.input.messages[0].content = "a".repeat(32001)),
  "content 16001 emoji": (i: Json) =>
    (i.input.messages[0].content = "😀".repeat(16001)),
  "role tool": (i: Json) => (i.input.messages[0].role = "tool"),
  "message extra key": (i: Json) => (i.input.messages[0].name = "x"),
  "101 messages": (i: Json) => {
    i.input.messages = Array.from({ length: 101 }, () => ({
      role: "user",
      content: "x",
    }));
  },
  "task unknown kind": (i: Json) =>
    (i.input = { task: { kind: "pdf", prompt: "x" } }),
  "task empty prompt": (i: Json) =>
    (i.input = { task: { kind: "image", prompt: "" } }),
  "max output zero": (i: Json) => (i.limits.max_output_tokens = 0),
  "max output 131073": (i: Json) => (i.limits.max_output_tokens = 131073),
  "max output fractional": (i: Json) => (i.limits.max_output_tokens = 1.5),
  "timeout 99": (i: Json) => (i.limits.timeout_ms = 99),
  "timeout 600001": (i: Json) => (i.limits.timeout_ms = 600001),
  "sampling empty": (i: Json) => (i.sampling = {}),
  "sampling unknown key": (i: Json) => (i.sampling = { top_k: 4 }),
  "sampling temperature 2.1": (i: Json) => (i.sampling = { temperature: 2.1 }),
  "sampling temperature negative": (i: Json) =>
    (i.sampling = { temperature: -0.1 }),
  "sampling top_p zero": (i: Json) => (i.sampling = { top_p: 0 }),
  "sampling top_p above one": (i: Json) => (i.sampling = { top_p: 1.01 }),
  "sampling stop empty list": (i: Json) => (i.sampling = { stop: [] }),
  "sampling stop five": (i: Json) =>
    (i.sampling = { stop: ["a", "b", "c", "d", "e"] }),
  "sampling stop empty string": (i: Json) => (i.sampling = { stop: [""] }),
  "sampling stop 65 characters": (i: Json) =>
    (i.sampling = { stop: ["a".repeat(65)] }),
  "sampling seed fractional": (i: Json) => (i.sampling = { seed: 1.5 }),
  "sampling seed above safe": (i: Json) =>
    (i.sampling = { seed: 9007199254740992 }),
  "sampling null": (i: Json) => (i.sampling = null),
  "reasoning effort unknown": (i: Json) => (i.reasoning_effort = "max"),
  "reasoning effort null": (i: Json) => (i.reasoning_effort = null),
  "cache mode unknown": (i: Json) => (i.cache = { mode: "always" }),
  "cache mode missing": (i: Json) => (i.cache = {}),
  "cache ttl is not a field": (i: Json) =>
    (i.cache = { mode: "auto", ttl: "1h" }),
  "cache null": (i: Json) => (i.cache = null),
  "cache is a string": (i: Json) => (i.cache = "auto"),
  "tags empty": (i: Json) => (i.tags = {}),
  "tags seventeen": (i: Json) =>
    (i.tags = Object.fromEntries(
      Array.from({ length: 17 }, (_, n) => [`k${n}`, "v"]),
    )),
  "tags key with space": (i: Json) => (i.tags = { "a b": "v" }),
  "tags key 65 characters": (i: Json) => (i.tags = { ["k".repeat(65)]: "v" }),
  "tags empty value": (i: Json) => (i.tags = { k: "" }),
  "tags value 257 characters": (i: Json) => (i.tags = { k: "v".repeat(257) }),
  "tags value with newline": (i: Json) => (i.tags = { k: "a\nb" }),
  "tags value not a string": (i: Json) => (i.tags = { k: 1 }),
  "tags as list": (i: Json) => (i.tags = ["a"]),
  "max charge scale 8": (i: Json) =>
    (i.limits.max_charge = { asset: "a", atoms: "1", scale: 8 }),
  "max charge atoms leading zero": (i: Json) =>
    (i.limits.max_charge = { asset: "a", atoms: "01", scale: 9 }),
  "max usage unknown meter": (i: Json) => (i.limits.max_usage = { pixels: 1 }),
  "max usage negative": (i: Json) => (i.limits.max_usage = { images: -1 }),
  "stream as string": (i: Json) => (i.stream = "yes"),
  "stream null": (i: Json) => (i.stream = null),
};
for (const [name, edit] of Object.entries(invocationMutations))
  invocation(name, edit);

const rootUrl = new URL("../conformance/contracts.v1.json", import.meta.url);
const rustUrl = new URL(
  "../backends/rust/conformance/contracts.v1.json",
  import.meta.url,
);
// The Rust copy is the reference's cases first, then the cases for features the reference does not have yet. Read both
// files as they are before writing: a case the previous root file had is regenerated, anything else in the Rust copy is
// an extra and is kept, in order, unless the reference now has a case of the same name.
const previousRoot = JSON.parse(readFileSync(rootUrl, "utf8"));
const previousRust = JSON.parse(readFileSync(rustUrl, "utf8"));
const merge = (kind: "service" | "invocation", fresh: Json[]) => {
  const generated = new Set<string>(
    previousRoot[kind].map((c: Json) => c.name),
  );
  const names = new Set(fresh.map((c) => c.name));
  const extras = previousRust[kind].filter(
    (c: Json) => !generated.has(c.name) && !names.has(c.name),
  );
  return [...fresh, ...extras];
};
const document = (service: Json[], invocation: Json[]) =>
  JSON.stringify(
    {
      version: 1,
      description: previousRoot.description,
      service,
      invocation,
    },
    null,
    1,
  ) + "\n";
writeFileSync(rootUrl, document(services, invocations));
writeFileSync(
  rustUrl,
  document(merge("service", services), merge("invocation", invocations)),
);
console.log(
  `contracts.v1.json: ${services.length} service and ${invocations.length} invocation cases`,
);
