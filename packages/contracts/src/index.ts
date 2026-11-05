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
export const Invocation = z
  .object({
    service: id,
    service_version: z.number().int().positive().optional(),
    customer_id: id,
    payer_mode: z.literal("developer").default("developer"),
    input: z
      .object({
        messages: z
          .array(
            z
              .object({
                role: z.enum(["system", "user", "assistant"]),
                content: z.string().min(1).max(32000),
              })
              .strict(),
          )
          .max(100)
          .default([]),
        task: z
          .object({ kind: TaskKind, prompt: z.string().min(1).max(32000) })
          .strict()
          .optional(),
      })
      .strict(),
    limits: z
      .object({
        max_output_tokens: z.number().int().min(1).max(8192).default(1),
        max_usage: MediaMeters.optional(),
        timeout_ms: z.number().int().min(100).max(120000).default(30000),
        max_charge: z
          .object({ asset: id, atoms, scale: z.literal(9) })
          .strict()
          .optional(),
      })
      .strict(),
    stream: z.boolean().default(false),
  })
  .strict();
export type Invocation = z.infer<typeof Invocation>;
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
    max_output_tokens: z.number().int().min(1).max(8192),
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
    max_output_tokens: z.number().int().positive().max(8192),
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
  | { type: "usage"; usage: Usage; upstream_id?: string }
  | { type: "done"; completion?: "succeeded" | "partial" };
export interface Provider {
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
  ) {
    super(code);
    this.name = "ProviderError";
  }
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
