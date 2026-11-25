import { z } from "zod";
export { z };
export const atoms = z.string().regex(/^(0|[1-9][0-9]{0,29})$/);
const id = z
  .string()
  .min(1)
  .max(120)
  .regex(/^[a-zA-Z0-9_.:/-]+$/);
export const TaskKind = z.enum(["image", "video", "audio", "task"]);
export const MediaMeters = z
  .object({
    images: z.number().int().nonnegative().safe().optional(),
    video_ms: z.number().int().nonnegative().safe().optional(),
    audio_ms: z.number().int().nonnegative().safe().optional(),
    tasks: z.number().int().nonnegative().safe().optional(),
  })
  .strict();
export const Meters = z
  .object({
    input: z.number().int().nonnegative().safe(),
    cached: z.number().int().nonnegative().safe(),
    cache_write: z.number().int().nonnegative().safe(),
    output: z.number().int().nonnegative().safe(),
    ...MediaMeters.shape,
  })
  .strict();
export type Usage = z.infer<typeof Meters>;
const MeterRate = z
  .object({ atoms, per: z.number().int().positive().max(1_000_000_000) })
  .strict();
export const Price = z
  .object({
    media: z
      .object({
        images: MeterRate.optional(),
        video_ms: MeterRate.optional(),
        audio_ms: MeterRate.optional(),
        tasks: MeterRate.optional(),
      })
      .strict()
      .optional(),
    asset: id,
    input: atoms,
    cached: atoms,
    cache_write: atoms,
    output: atoms,
    per: z.number().int().positive().max(1_000_000_000).default(1),
  })
  .strict();
export type Price = z.infer<typeof Price>;
const jsonDepth = (value: unknown, limit: number): boolean => {
  if (limit < 0) return false;
  if (Array.isArray(value)) return value.every((v) => jsonDepth(v, limit - 1));
  if (value && typeof value === "object")
    return Object.values(value).every((v) => jsonDepth(v, limit - 1));
  return true;
};
const isJsonObject = (text: string): boolean => {
  try {
    const value: unknown = JSON.parse(text);
    return typeof value === "object" && value !== null && !Array.isArray(value);
  } catch {
    return false;
  }
};
const toolName = z.string().regex(/^[a-zA-Z0-9_-]{1,64}$/);
const toolCallId = z.string().regex(/^[a-zA-Z0-9_-]{1,128}$/);
export const ToolCall = z
  .object({
    id: toolCallId,
    name: toolName,
    arguments: z.string().max(256000).refine(isJsonObject),
  })
  .strict();
export type ToolCall = z.infer<typeof ToolCall>;
export const Tool = z
  .object({
    name: toolName,
    description: z.string().min(1).max(4096).optional(),
    input_schema: z
      .record(z.string(), z.unknown())
      .refine((s) => s.type === "object" && jsonDepth(s, 32)),
  })
  .strict();
export type Tool = z.infer<typeof Tool>;
export const ToolChoice = z.union([
  z.enum(["auto", "none", "required"]),
  z.object({ name: toolName }).strict(),
]);
export type ToolChoice = z.infer<typeof ToolChoice>;
const imageUrl = z
  .string()
  .max(3_000_000)
  .refine(
    (url) =>
      /^data:image\/(png|jpeg|gif|webp);base64,[A-Za-z0-9+/]+={0,2}$/.test(
        url,
      ) ||
      (url.length <= 4096 && /^https:\/\/[^\s]+$/.test(url)),
  );
export const ContentPart = z.union([
  z
    .object({ type: z.literal("text"), text: z.string().min(1).max(32000) })
    .strict(),
  z
    .object({
      type: z.literal("image"),
      url: imageUrl,
      detail: z.enum(["auto", "low", "high"]).optional(),
    })
    .strict(),
]);
export type ContentPart = z.infer<typeof ContentPart>;
export const InputMessage = z.union([
  z
    .object({
      role: z.enum(["system", "user", "assistant"]),
      content: z.string().min(1).max(32000),
    })
    .strict(),
  z
    .object({
      role: z.literal("user"),
      content: z.array(ContentPart).min(1).max(32),
    })
    .strict(),
  z
    .object({
      role: z.literal("assistant"),
      content: z.string().min(1).max(32000).optional(),
      tool_calls: z.array(ToolCall).min(1).max(64),
    })
    .strict(),
  z
    .object({
      role: z.literal("tool"),
      tool_call_id: toolCallId,
      content: z.string().max(256000),
      is_error: z.boolean().optional(),
    })
    .strict(),
]);
export type InputMessage = z.infer<typeof InputMessage>;
/** The largest output a single model call may be asked for: wide enough for today's long-output models (128,000). */
export const MAX_OUTPUT_TOKENS = 131072;
/** The longest a request may run, in milliseconds: long enough for a slow reasoning stream. */
export const MAX_TIMEOUT_MS = 600000;
const noControlCharacters = (text: string): boolean =>
  !/[\u0000-\u001f\u007f]/.test(text);
/**
 * Sampling controls. Every field is optional and a provider that cannot carry one refuses the request before anything
 * is reserved (`unsupported_capability`) rather than ignoring it.
 */
export const Sampling = z
  .object({
    temperature: z.number().min(0).max(2).optional(),
    top_p: z.number().gt(0).max(1).optional(),
    stop: z.array(z.string().min(1).max(64)).min(1).max(4).optional(),
    seed: z.number().int().safe().optional(),
  })
  .strict()
  .refine((s) => Object.keys(s).length > 0);
export type Sampling = z.infer<typeof Sampling>;
export const ReasoningEffort = z.enum(["low", "medium", "high"]);
export type ReasoningEffort = z.infer<typeof ReasoningEffort>;
/** Free-form labels for attribution: one to sixteen name/value pairs, never interpreted by the router. */
export const Tags = z
  .record(
    z.string().regex(/^[A-Za-z0-9_.:-]{1,64}$/),
    z.string().min(1).max(256).refine(noControlCharacters),
  )
  .refine((t) => {
    const n = Object.keys(t).length;
    return n >= 1 && n <= 16;
  });
export type Tags = z.infer<typeof Tags>;
/** An enforced JSON Schema response: the answer is a JSON document that follows `schema`. */
export const ResponseFormat = z
  .object({
    type: z.literal("json_schema"),
    json_schema: z
      .object({
        name: z.string().regex(/^[a-zA-Z0-9_-]{1,64}$/),
        description: z.string().max(1024).optional(),
        schema: z.record(z.string(), z.unknown()),
        // A schema is always enforced: `false` would ask for a best effort the router cannot promise.
        strict: z.literal(true).optional(),
      })
      .strict(),
  })
  .strict();
export type ResponseFormat = z.infer<typeof ResponseFormat>;
export const Invocation = z
  .object({
    response_format: ResponseFormat.optional(),
    service: id,
    service_version: z.number().int().positive().optional(),
    customer_id: id,
    payer_mode: z.literal("developer").default("developer"),
    input: z
      .object({
        messages: z.array(InputMessage).max(100).default([]),
        // No defaults here: a request without tools must keep its exact canonical form and idempotency digest.
        tools: z.array(Tool).min(1).max(64).optional(),
        tool_choice: ToolChoice.optional(),
        task: z
          .object({ kind: TaskKind, prompt: z.string().min(1).max(32000) })
          .strict()
          .optional(),
      })
      .strict(),
    limits: z
      .object({
        max_output_tokens: z
          .number()
          .int()
          .min(1)
          .max(MAX_OUTPUT_TOKENS)
          .default(1),
        max_usage: MediaMeters.optional(),
        timeout_ms: z
          .number()
          .int()
          .min(100)
          .max(MAX_TIMEOUT_MS)
          .default(30000),
        max_charge: z
          .object({ asset: id, atoms, scale: z.literal(9) })
          .strict()
          .optional(),
      })
      .strict(),
    // No defaults on these three: a request without them keeps its exact canonical form and idempotency digest.
    sampling: Sampling.optional(),
    reasoning_effort: ReasoningEffort.optional(),
    tags: Tags.optional(),
    stream: z.boolean().default(false),
  })
  .strict()
  .superRefine((value, c) => {
    const { messages, tools, tool_choice: choice, task } = value.input;
    const issue = (message: string) => c.addIssue({ code: "custom", message });
    const names = new Set(tools?.map((t) => t.name));
    if (tools && names.size !== tools.length) issue("duplicate tool name");
    if (task && (tools || choice)) issue("a task cannot declare tools");
    if (task && value.response_format)
      issue("a task cannot ask for structured output");
    if (choice !== undefined && !tools) issue("tool_choice requires tools");
    if (typeof choice === "object" && !names.has(choice.name))
      issue("tool_choice names an undeclared tool");
    // Every result answers an earlier call, and no call is left unanswered: upstreams refuse both.
    const seen = new Set<string>();
    const open = new Set<string>();
    for (const m of messages) {
      if (m.role === "tool") {
        if (!open.delete(m.tool_call_id)) issue("unmatched tool result");
        continue;
      }
      if (open.size) issue("unanswered tool call");
      open.clear();
      if (m.role === "assistant" && "tool_calls" in m)
        for (const call of m.tool_calls) {
          if (seen.has(call.id)) issue("duplicate tool call id");
          seen.add(call.id);
          open.add(call.id);
        }
    }
    if (open.size) issue("unanswered tool call");
  });
export type Invocation = z.infer<typeof Invocation>;
/** What a request needs from a provider beyond plain text. */
export interface Needs {
  tools: boolean;
  images: boolean;
  /** The request carries a `response_format`: the answer must follow a JSON Schema. */
  structured_output: boolean;
  temperature: boolean;
  top_p: boolean;
  stop: boolean;
  seed: boolean;
  reasoning_effort: boolean;
}
export function invocationNeeds(input: Invocation): Needs {
  const messages = input.input.messages;
  return {
    tools:
      input.input.tools !== undefined ||
      messages.some((m) => m.role === "tool" || "tool_calls" in m),
    images: messages.some(
      (m) =>
        Array.isArray(m.content) && m.content.some((p) => p.type === "image"),
    ),
    structured_output: input.response_format !== undefined,
    temperature: input.sampling?.temperature !== undefined,
    top_p: input.sampling?.top_p !== undefined,
    stop: input.sampling?.stop !== undefined,
    seed: input.sampling?.seed !== undefined,
    reasoning_effort: input.reasoning_effort !== undefined,
  };
}
export const Offering = z
  .object({
    id,
    task_kind: TaskKind.optional(),
    max_usage: MediaMeters.optional(),
    provider: id,
    model: id,
    upstream_model: id,
    region: id,
    labels: z.array(id).default([]),
    enabled: z.boolean(),
    source: z.enum(["platform", "byok"]),
    max_input_tokens: z.number().int().min(1).max(1000000),
    max_output_tokens: z.number().int().min(1).max(MAX_OUTPUT_TOKENS),
    price: Price,
    latency_ms: z.number().nonnegative().optional(),
    reliability: z.number().min(0).max(1).optional(),
    quality: z.number().min(0).max(1).optional(),
  })
  .strict();
export type Offering = z.infer<typeof Offering>;
export const Service = z
  .object({
    id,
    version: z.number().int().positive(),
    task_kind: TaskKind.optional(),
    mode: z.enum(["specific", "pool", "tier"]),
    model: id.optional(),
    tier: id.optional(),
    offerings: z.array(Offering).min(1),
    regions: z.array(id).min(1),
    required_labels: z.array(id).default([]),
    price: Price,
    platform_price: Price.optional(),
    requires_subscription: z.boolean().optional(),
    max_attempts: z.number().int().min(1).max(5),
    max_provider_cost: atoms,
    max_output_tokens: z.number().int().positive().max(MAX_OUTPUT_TOKENS),
    weights: z
      .object({
        cost: z.number().min(0),
        latency: z.number().min(0),
        reliability: z.number().min(0),
        quality: z.number().min(0),
      })
      .strict(),
    cost_scale: atoms.refine((x) => BigInt(x) > 0n),
    latency_scale_ms: z.number().positive(),
    unknown_score: z.number().min(0).max(1).default(0),
  })
  .strict()
  .superRefine((s, c) => {
    if (
      Math.abs(Object.values(s.weights).reduce((a, b) => a + b, 0) - 1) > 1e-9
    )
      c.addIssue({ code: "custom", message: "weights must sum to one" });
    if (s.mode === "specific" && !s.model)
      c.addIssue({ code: "custom", message: "specific requires model" });
    if (s.mode === "tier" && !s.tier)
      c.addIssue({ code: "custom", message: "tier requires name" });
    if (new Set(s.offerings.map((x) => x.id)).size !== s.offerings.length)
      c.addIssue({ code: "custom", message: "duplicate offering" });
    if (new Set(s.offerings.map((x) => x.price.asset)).size !== 1)
      c.addIssue({ code: "custom", message: "supply currency mismatch" });
  });
export type Service = z.infer<typeof Service>;
export interface Principal {
  workspace: string;
  environment: string;
  customer: string;
  scopes: string[];
  auth_key_id?: string;
  member_id?: string;
  service_ids?: string[];
}
export type ProviderEvent =
  | { type: "delta"; text: string }
  /** Part of a tool call. `id` and `name` are required on the first event of each `index`. */
  | {
      type: "tool_call";
      index: number;
      id?: string;
      name?: string;
      arguments_delta: string;
    }
  | { type: "usage"; usage: Usage; upstream_id?: string }
  | { type: "done"; completion?: "succeeded" | "partial" };
/** What a provider can carry besides text. Absent means text only, so such a request is refused before any charge. */
export interface ProviderCapabilities {
  tools?: boolean;
  images?: boolean;
  /** Writes a request's `response_format` schema into its call. A provider that does not say so is never sent one. */
  structured_output?: boolean;
  /** Each sampling control and the reasoning effort is carried only when the provider says so. */
  temperature?: boolean;
  top_p?: boolean;
  stop?: boolean;
  seed?: boolean;
  reasoning_effort?: boolean;
}
export interface Provider {
  capabilities?: ProviderCapabilities;
  invoke(
    input: Invocation,
    offering: Offering,
    signal: AbortSignal,
  ): AsyncIterable<ProviderEvent>;
}
export class RouterError extends Error {
  request_id?: string;
  constructor(
    public code: string,
    public status = 400,
    public retryable = false,
    /** Safe, customer-visible facts about the refusal (for example which window and when it resets). */
    public details?: Record<string, unknown>,
  ) {
    super(code);
    this.name = "RouterError";
  }
}
export class ProviderError extends Error {
  constructor(
    public definitive: boolean,
    public retryable: boolean,
    public code = "upstream_failed",
    /** How long the provider asked callers to wait (its `Retry-After`), in milliseconds. Only a hint. */
    public retry_after_ms?: number,
  ) {
    super(code);
    this.name = "ProviderError";
  }
}
/**
 * What an upstream refusal means for the next offering. A failure of the **request** (malformed, too large) would be
 * refused by every offering, so the call stops. A failure of the **offering** (its key was rejected, its model is gone,
 * its account is empty, its region is blocked, it is rate limited) is certain to have done no work and another offering
 * may serve the request. Server errors and unknown statuses are neither: the provider may have started work, so the
 * reservation stays held and the call is never retried for free.
 */
export type FailureClass =
  | "rate_limited"
  | "quota_exhausted"
  | "credentials_rejected"
  | "model_unavailable"
  | "region_blocked"
  | "request_rejected"
  | "unknown";
// Phrases are written with spaces: the message is lower-cased and its underscores and hyphens read as spaces first.
const CREDENTIAL_PHRASES = [
  "api key not valid",
  "api key expired",
  "api key invalid",
  "invalid api key",
  "incorrect api key",
  "invalid x api key",
];
const QUOTA_PHRASES = [
  "insufficient quota",
  "exceeded your current quota",
  "credit balance is too low",
  "credits are depleted",
  "spend limit",
  "spending cap",
  "requires billing to be enabled",
  "billing account",
  "per day",
];
const REGION_PHRASES = [
  "user location is not supported",
  "not available in your country",
  "not available in your region",
  "not supported in your region",
];
const MODEL_PHRASES = [
  "model not found",
  "is not found for api version",
  "does not exist or you do not have access",
];
/** Classifies a refusal from its HTTP status and the error's own message (may be empty). */
export function classifyFailure(status: number, message: string): FailureClass {
  const text = message.toLowerCase().replace(/[_-]/g, " ");
  const said = (phrases: string[]) => phrases.some((p) => text.includes(p));
  if (said(CREDENTIAL_PHRASES)) return "credentials_rejected";
  if (said(REGION_PHRASES)) return "region_blocked";
  if (status === 401) return "credentials_rejected";
  if (status === 402) return "quota_exhausted";
  if (
    (status === 403 || status === 429 || status === 400) &&
    said(QUOTA_PHRASES)
  )
    return "quota_exhausted";
  if (status === 403) return "credentials_rejected";
  if (status === 404) return "model_unavailable";
  if (status === 429) return "rate_limited";
  if (status === 400 && said(MODEL_PHRASES)) return "model_unavailable";
  // Any other client error is the request's: 408 (a timeout the provider may have acted on) is not.
  if (status >= 400 && status < 500 && status !== 408)
    return "request_rejected";
  return "unknown";
}
/** Whether a failure belongs to the offering, so another may be tried and this one avoided for a while. */
export function isOfferingScoped(c: FailureClass): boolean {
  return (
    c === "rate_limited" ||
    c === "quota_exhausted" ||
    c === "credentials_rejected" ||
    c === "model_unavailable" ||
    c === "region_blocked"
  );
}
/** The provider error for a class: offering-scoped is definitive and retryable, a rejected request is definitive and not, anything else keeps the hold. */
export function failureError(
  c: FailureClass,
  status: number,
  retryAfterMs?: number,
): ProviderError {
  if (isOfferingScoped(c))
    return new ProviderError(true, true, c, retryAfterMs);
  return new ProviderError(
    c === "request_rejected",
    false,
    `upstream_http_${status}`,
    retryAfterMs,
  );
}
/** The longest wait a `Retry-After` can ask for, in milliseconds: a hostile or broken value cannot park an offering for days. */
export const RETRY_AFTER_MAX_MS = 86_400_000;
/**
 * Reads the value of a `Retry-After` header as milliseconds. Only the delta-seconds form is read: a non-negative number
 * of seconds, whole or with a fraction, surrounding spaces ignored, milliseconds rounded down, capped at a day. The
 * HTTP-date form is not read (`undefined`).
 */
export function parseRetryAfter(value: string | null): number | undefined {
  // JavaScript's trim also strips characters (U+FEFF, U+2028) that the other languages' trim does not; the header
  // grammar allows only spaces and tabs around the value.
  const v = (value ?? "").replace(/^[ \t]+|[ \t]+$/g, "");
  const m = /^([0-9]+)(?:\.([0-9]+))?$/.exec(v);
  if (!m) return undefined;
  const seconds = m[1].length > 9 ? RETRY_AFTER_MAX_MS / 1000 : Number(m[1]);
  const millis = Number((m[2] ?? "").slice(0, 3).padEnd(3, "0"));
  return Math.min(seconds * 1000 + millis, RETRY_AFTER_MAX_MS);
}
/**
 * The provider error for an HTTP refusal: the status, the first 16 KiB of a client error's body (only its
 * `error.message` is read, nothing of it is passed on) and the `Retry-After` header. Server errors are not read: the
 * class cannot change what they mean.
 */
export async function refusal(response: Response): Promise<ProviderError> {
  const status = response.status;
  const retryAfter = parseRetryAfter(response.headers.get("retry-after"));
  let message = "";
  if (status >= 400 && status < 500 && response.body) {
    try {
      const reader = response.body.getReader();
      const chunks: Uint8Array[] = [];
      let size = 0;
      while (size < 16384) {
        const { done, value } = await reader.read();
        if (done) break;
        chunks.push(value);
        size += value.length;
      }
      await reader.cancel().catch(() => undefined);
      const bytes = new Uint8Array(size);
      let at = 0;
      for (const chunk of chunks) {
        bytes.set(chunk, at);
        at += chunk.length;
      }
      const text = new TextDecoder().decode(bytes.subarray(0, 16384));
      const parsed: unknown = JSON.parse(text);
      const m = (parsed as { error?: { message?: unknown } } | null)?.error
        ?.message;
      if (typeof m === "string") message = m;
    } catch {
      // A body that is not JSON says nothing; the status alone decides.
    }
  } else await response.body?.cancel().catch(() => undefined);
  return failureError(classifyFailure(status, message), status, retryAfter);
}
export interface StreamEvent {
  type: string;
  request_id: string;
  sequence: number;
  [key: string]: unknown;
}
export const EventSchema = z
  .object({
    id,
    type: z.enum([
      "billing.settled",
      "billing.released",
      "reconciliation.required",
    ]),
    schema_version: z.literal(1),
    workspace_id: id,
    environment_id: id,
    request_id: id,
    occurred_at: z.string().datetime(),
  })
  .strict();
export type DomainEvent = z.infer<typeof EventSchema>;

/** Parse SSE across arbitrary chunk and CRLF boundaries. Content is never logged. */
export async function* parseSSE(
  body: ReadableStream<Uint8Array>,
): AsyncGenerator<{ event: string; data: string }> {
  const reader = body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";
  try {
    while (true) {
      const { done, value } = await reader.read();
      buffer += decoder.decode(value, { stream: !done });
      if (buffer.length > 2_000_000)
        throw new ProviderError(false, false, "stream_frame_too_large");
      let match: RegExpExecArray | null;
      while ((match = /\r?\n\r?\n/.exec(buffer))) {
        const frame = buffer.slice(0, match.index);
        buffer = buffer.slice(match.index + match[0].length);
        let event = "message";
        const data: string[] = [];
        for (const line of frame.split(/\r?\n/)) {
          if (line.startsWith("event:")) event = line.slice(6).trim();
          if (line.startsWith("data:"))
            data.push(line.slice(5).replace(/^ /, ""));
        }
        if (data.length) yield { event, data: data.join("\n") };
      }
      if (done) break;
    }
  } finally {
    await reader.cancel().catch(() => {});
    reader.releaseLock();
  }
}
export * from "./control.js";
