import { createHmac, randomUUID } from "node:crypto";
import {
  Invocation,
  Meters,
  Service,
  RouterError,
  ProviderError,
  invocationNeeds,
  type Principal,
  type Provider,
  type Offering,
  type Usage,
  type Price,
  type StreamEvent,
} from "@supersolutions/super-router-contracts";
export { RouterError };
export interface Leg {
  kind: "customer_sale" | "platform_sale";
  account: string;
  price: Price;
  reserved: string;
}
export interface RequestRecord {
  id: string;
  workspace: string;
  environment: string;
  customer: string;
  service: Service;
  execution: string;
  billing: string;
  cancel_requested: boolean;
  output_started: boolean;
  deadline: string;
  legs: Leg[];
  provider_spent: string;
}
export interface Store {
  replay(
    p: Principal,
    key: string,
    digest: string,
  ): Promise<RequestRecord | null>;
  getService(p: Principal, id: string, version?: number): Promise<Service>;
  accounts(p: Principal, service: Service): Promise<Leg[]>;
  reserve(
    p: Principal,
    key: string,
    digest: string,
    service: Service,
    legs: Leg[],
    deadline: string,
    decision?: unknown,
  ): Promise<{ request: RequestRecord; fresh: boolean }>;
  get(p: Principal, id: string): Promise<RequestRecord>;
  begin(
    p: Principal,
    id: string,
    offering: Offering,
    bound: string,
  ): Promise<string>;
  output(p: Principal, id: string): Promise<void>;
  attemptResult(
    p: Principal,
    id: string,
    attempt: string,
    status: string,
    usage: Usage | null,
    cost: string | null,
    upstream?: string,
  ): Promise<void>;
  finish(
    p: Principal,
    id: string,
    execution: string,
    usage: Usage | null,
    pending: boolean,
  ): Promise<void>;
  cancel(p: Principal, id: string): Promise<void>;
}
export interface BalanceRow {
  id: string;
  kind: string;
  asset: string;
  balance: string;
  held: string;
  available: string;
}
/** A source of epoch milliseconds. Stores and the router accept one so tests and conformance runs control time. */
export type Clock = () => number;
/**
 * Everything the data-plane server and worker need from storage, beyond the router's own {@link Store} port.
 * Implement this to plug in another database; run the store contract suite against it.
 */
export interface DataStore extends Store {
  publish(
    p: Principal,
    service: unknown,
    mode?: "cloud" | "self-host",
    expectedVersion?: number,
  ): Promise<Service | undefined>;
  list(p: Principal): Promise<Service[]>;
  balance(p: Principal): Promise<BalanceRow[]>;
  grant(
    p: Principal,
    kind: "customer_sale" | "platform_sale",
    asset: string,
    amount: string,
    source: string,
    expires?: Date,
  ): Promise<string>;
  disable(p: Principal, offering: string): Promise<void>;
  /** Settle or release expired reservations. Returns how many requests were processed. */
  recover(): Promise<number>;
  /** Optional operator view: recent requests with attempts and settled amounts. */
  recent?(
    p: Principal,
    limit?: number,
    customer?: string,
  ): Promise<RecentRequest[]>;
}
export interface RecentRequest {
  id: string;
  customer: string;
  service: string;
  service_version: number;
  execution: string;
  billing: string;
  provider_cost: string;
  created_at: Date;
  attempts: {
    offering: string;
    status: string;
    cost: string | null;
    usage: unknown;
  }[];
  settled: Record<string, string>;
}
export const mediaKeys = ["images", "video_ms", "audio_ms", "tasks"] as const;
export const kindMeter = {
  image: "images",
  video: "video_ms",
  audio: "audio_ms",
  task: "tasks",
} as const;
export function charge(usage: Usage, price: Price): bigint {
  const numerator = (
    ["input", "cached", "cache_write", "output"] as const
  ).reduce((sum, key) => sum + BigInt(usage[key]) * BigInt(price[key]), 0n);
  let total = (numerator + BigInt(price.per) - 1n) / BigInt(price.per);
  for (const key of mediaKeys) {
    const units = usage[key] ?? 0;
    const rate = price.media?.[key];
    if (units && !rate) throw new RouterError("unpriced_meter", 422);
    if (rate)
      total +=
        (BigInt(units) * BigInt(rate.atoms) + BigInt(rate.per) - 1n) /
        BigInt(rate.per);
  }
  return total;
}
function invocationBound(
  input: Invocation,
  maxInput: number,
  price: Price,
): bigint {
  if (!input.input.task)
    return bound(maxInput, input.limits.max_output_tokens, price);
  return charge(
    {
      input: 0,
      cached: 0,
      cache_write: 0,
      output: 0,
      ...input.limits.max_usage,
    },
    price,
  );
}
export function bound(input: number, output: number, price: Price): bigint {
  const rate = [price.input, price.cached, price.cache_write]
    .map(BigInt)
    .reduce((a, b) => (a > b ? a : b));
  return (
    (BigInt(input) * rate +
      BigInt(output) * BigInt(price.output) +
      BigInt(price.per) -
      1n) /
    BigInt(price.per)
  );
}
/** The canonical JSON of a value: object keys sorted, no whitespace. It is the input of the idempotency digest. */
/**
 * Orders two strings by Unicode code point, which is also UTF-8 byte order. `localeCompare` depends on the
 * runtime's locale and puts `userName` after `user_id`, so a tool schema with mixed-case keys would digest
 * differently here than in the Go, Rust and Python ports, and differently between two Node installs.
 */
function byCodePoint(a: string, b: string): number {
  let i = 0;
  let j = 0;
  while (i < a.length && j < b.length) {
    const x = a.codePointAt(i)!;
    const y = b.codePointAt(j)!;
    if (x !== y) return x - y;
    i += x > 0xffff ? 2 : 1;
    j += y > 0xffff ? 2 : 1;
  }
  return a.length - i - (b.length - j);
}
export function canonical(value: unknown): string {
  if (Array.isArray(value)) return "[" + value.map(canonical).join(",") + "]";
  if (value && typeof value === "object")
    return (
      "{" +
      Object.entries(value)
        .sort(([a], [b]) => byCodePoint(a, b))
        .map(([k, v]) => JSON.stringify(k) + ":" + canonical(v))
        .join(",") +
      "}"
    );
  return JSON.stringify(value);
}
export function route(service: Service, input: Invocation) {
  const excluded: { id: string; reason: string }[] = [];
  const candidates = service.offerings
    .flatMap((o) => {
      let reason = "";
      if (
        o.task_kind !== input.input.task?.kind ||
        service.task_kind !== input.input.task?.kind
      )
        reason = "task_kind_mismatch";
      else if (!o.enabled) reason = "disabled";
      else if (service.mode === "specific" && o.model !== service.model)
        reason = "model_mismatch";
      else if (!service.regions.includes(o.region)) reason = "region_mismatch";
      else if (service.required_labels.some((x) => !o.labels.includes(x)))
        reason = "label_mismatch";
      else if (input.limits.max_output_tokens > o.max_output_tokens)
        reason = "output_limit";
      if (!reason && input.input.task) {
        const key = kindMeter[input.input.task.kind];
        const units = input.limits.max_usage?.[key];
        if (
          !units ||
          units > (o.max_usage?.[key] ?? 0) ||
          !o.price.media?.[key]
        )
          reason = "meter_limit";
      }
      const cost = reason
        ? 0n
        : invocationBound(input, o.max_input_tokens, o.price);
      if (!reason && cost > BigInt(service.max_provider_cost))
        reason = "budget_exceeded";
      if (reason) {
        excluded.push({ id: o.id, reason });
        return [];
      }
      const ratio =
        Number((cost * 1_000_000n) / BigInt(service.cost_scale)) / 1_000_000;
      const score =
        service.weights.cost * (1 - Math.min(1, ratio)) +
        service.weights.latency *
          (o.latency_ms === undefined
            ? service.unknown_score
            : 1 - Math.min(1, o.latency_ms / service.latency_scale_ms)) +
        service.weights.reliability * (o.reliability ?? service.unknown_score) +
        service.weights.quality * (o.quality ?? service.unknown_score);
      return [{ offering: o, cost, score }];
    })
    .sort(
      (a, b) => b.score - a.score || a.offering.id.localeCompare(b.offering.id),
    );
  return { candidates, excluded };
}
export class Router {
  constructor(
    public store: Store,
    private providers: Record<string, Provider>,
    private digestKey: string,
    private clock: Clock = Date.now,
  ) {
    if (digestKey.length < 32)
      throw new Error("digest key must be >=32 characters");
  }
  /** Own providers only: an offering may name its provider `constructor` or `toString`, which `providers[name]` finds on Object.prototype. */
  private providerFor(name: string): Provider | undefined {
    return Object.hasOwn(this.providers, name)
      ? this.providers[name]
      : undefined;
  }
  async infer(
    principal: Principal,
    raw: unknown,
    key: string,
    emit?: (event: StreamEvent) => Promise<void>,
    signal?: AbortSignal,
  ) {
    if (!principal.scopes.includes("invoke"))
      throw new RouterError("scope_denied", 403);
    const parsed = Invocation.safeParse(raw);
    if (!parsed.success) throw new RouterError("invalid_request", 400);
    const input = parsed.data;
    if (input.input.task) {
      const meter = kindMeter[input.input.task.kind];
      const limits = input.limits.max_usage;
      if (
        input.input.messages.length ||
        !limits?.[meter] ||
        mediaKeys.some((k) => k !== meter && (limits[k] ?? 0) !== 0) ||
        (meter === "tasks" && limits.tasks !== 1)
      )
        throw new RouterError("invalid_task_limits", 400);
    } else if (!input.input.messages.length || input.limits.max_usage)
      throw new RouterError("invalid_request", 400);
    if (principal.service_ids && !principal.service_ids.includes(input.service))
      throw new RouterError("scope_denied", 403);
    if (input.customer_id !== principal.customer)
      throw new RouterError("scope_denied", 403);
    if (!key || key.length > 200)
      throw new RouterError("missing_idempotency_key", 400);
    const digest = createHmac("sha256", this.digestKey)
      .update(canonical(input))
      .digest("hex");
    const existing = await this.store.replay(principal, key, digest);
    if (existing)
      return {
        replayed: true,
        id: existing.id,
        execution_status: existing.execution,
        billing_status: existing.billing,
      };
    const service = await this.store.getService(
      principal,
      input.service,
      input.service_version,
    );
    if (input.service_version !== undefined) {
      const active = await this.store.getService(principal, input.service);
      if (active.version !== service.version)
        throw new RouterError("inactive_service_version", 409);
    }
    if (input.limits.max_output_tokens > service.max_output_tokens)
      throw new RouterError("unsupported_capability", 422);
    const decision = route(service, input);
    if (!decision.candidates.length)
      throw new RouterError("no_eligible_offering", 422);
    if (decision.candidates.some((c) => !this.providerFor(c.offering.provider)))
      throw new RouterError("provider_unavailable", 503);
    // Refused here, before anything is reserved: a provider that cannot carry tools or images would drop them.
    const needs = invocationNeeds(input);
    const candidates = decision.candidates.filter((c) => {
      const can = this.providerFor(c.offering.provider)?.capabilities;
      const ok = (!needs.tools || can?.tools) && (!needs.images || can?.images);
      if (!ok)
        decision.excluded.push({
          id: c.offering.id,
          reason: "capability_unsupported",
        });
      return ok;
    });
    if (!candidates.length)
      throw new RouterError("unsupported_capability", 422);
    const maxInput = Math.max(
      ...candidates.map((c) => c.offering.max_input_tokens),
    );
    const legs = await this.store.accounts(principal, service);
    for (const leg of legs) {
      leg.reserved = invocationBound(input, maxInput, leg.price).toString();
      if (
        leg.kind === "customer_sale" &&
        input.limits.max_charge &&
        (input.limits.max_charge.asset !== leg.price.asset ||
          BigInt(input.limits.max_charge.atoms) < BigInt(leg.reserved))
      )
        throw new RouterError("budget_exceeded", 402);
    }
    const reservation = await this.store.reserve(
      principal,
      key,
      digest,
      service,
      legs,
      new Date(this.clock() + input.limits.timeout_ms).toISOString(),
      {
        excluded: decision.excluded,
        candidates: candidates.map((c) => ({
          id: c.offering.id,
          score: c.score,
          cost: c.cost.toString(),
        })),
      },
    );
    const request = reservation.request;
    if (!reservation.fresh)
      return {
        replayed: true,
        id: request.id,
        execution_status: request.execution,
        billing_status: request.billing,
      };
    let seq = 0,
      text = "",
      started = false,
      finalUsage: Usage | null = null,
      execution = "failed",
      pending = false,
      lastModel: string | undefined,
      toolBytes = 0;
    const toolCalls = new Map<
      number,
      { id: string; name: string; arguments: string }
    >();
    const send = async (type: string, data: Record<string, unknown> = {}) => {
      await emit?.({ type, request_id: request.id, sequence: ++seq, ...data });
    };
    const controller = new AbortController();
    const abort = () => controller.abort();
    signal?.addEventListener("abort", abort, { once: true });
    if (signal?.aborted) abort();
    const timeout = setTimeout(abort, input.limits.timeout_ms);
    let polling = false;
    const poll = setInterval(() => {
      if (polling) return;
      polling = true;
      void this.store
        .get(principal, request.id)
        .then((r) => {
          if (r.cancel_requested) abort();
        })
        .catch(abort)
        .finally(() => {
          polling = false;
        });
    }, 100);
    poll.unref();
    try {
      await send("request.started", { service_version: service.version });
      for (const candidate of candidates.slice(0, service.max_attempts)) {
        if (controller.signal.aborted) {
          execution = "cancelled";
          break;
        }
        const provider = this.providerFor(candidate.offering.provider);
        if (!provider) throw new RouterError("provider_unavailable", 503);
        let attempt: string;
        try {
          attempt = await this.store.begin(
            principal,
            request.id,
            candidate.offering,
            candidate.cost.toString(),
          );
        } catch (e) {
          if (
            e instanceof RouterError &&
            ["budget_exceeded", "connection_disabled", "cancelled"].includes(
              e.code,
            )
          ) {
            execution = e.code === "cancelled" ? "cancelled" : "failed";
            continue;
          }
          throw e;
        }
        let usage: Usage | null = null,
          upstream: string | undefined,
          done = false,
          completion: "succeeded" | "partial" = "succeeded";
        lastModel = candidate.offering.model;
        try {
          for await (const event of provider.invoke(
            input,
            candidate.offering,
            controller.signal,
          )) {
            if (event.type === "delta") {
              if (!started) {
                await this.store.output(principal, request.id);
                started = true;
              }
              text += event.text;
              // One cap on everything the model produced: text and tool-call arguments together.
              if (Buffer.byteLength(text) + toolBytes > 1_000_000)
                throw new ProviderError(false, false, "output_too_large");
              await send("output.delta", { text: event.text });
            } else if (event.type === "tool_call") {
              const known = toolCalls.get(event.index);
              if (
                !input.input.tools ||
                !Number.isSafeInteger(event.index) ||
                event.index < 0 ||
                (!known && (toolCalls.size >= 64 || !event.id || !event.name))
              )
                throw new ProviderError(false, false, "invalid_tool_call");
              // A tool call is output: after this point a failure is partial and never falls back to another offering.
              if (!started) {
                await this.store.output(principal, request.id);
                started = true;
              }
              toolBytes += Buffer.byteLength(event.arguments_delta);
              if (Buffer.byteLength(text) + toolBytes > 1_000_000)
                throw new ProviderError(false, false, "output_too_large");
              if (known) known.arguments += event.arguments_delta;
              else
                toolCalls.set(event.index, {
                  id: event.id!,
                  name: event.name!,
                  arguments: event.arguments_delta,
                });
              await send("output.tool_call.delta", {
                index: event.index,
                ...(known ? {} : { id: event.id, name: event.name }),
                arguments_delta: event.arguments_delta,
              });
            } else if (event.type === "usage") {
              const parsedUsage = Meters.safeParse(event.usage);
              if (!parsedUsage.success)
                throw new ProviderError(false, false, "invalid_usage");
              usage = parsedUsage.data;
              upstream = event.upstream_id;
            } else {
              done = true;
              completion = event.completion ?? "succeeded";
            }
          }
          if (!done || !usage)
            throw new ProviderError(false, false, "usage_unknown");
          if (input.input.task) {
            const meter = kindMeter[input.input.task.kind];
            if (
              ["input", "cached", "cache_write", "output"].some(
                (k) => usage![k as "input"] !== 0,
              ) ||
              mediaKeys.some(
                (k) => (usage![k] ?? 0) > (input.limits.max_usage?.[k] ?? 0),
              ) ||
              (meter === "tasks" &&
                usage.tasks !== (completion === "succeeded" ? 1 : 0))
            ) {
              usage = null;
              throw new ProviderError(false, false, "usage_out_of_bounds");
            }
          } else if (mediaKeys.some((k) => (usage![k] ?? 0) !== 0)) {
            usage = null;
            throw new ProviderError(false, false, "unexpected_meter");
          }
          await this.store.attemptResult(
            principal,
            request.id,
            attempt,
            completion,
            usage,
            charge(usage, candidate.offering.price).toString(),
            upstream,
          );
          finalUsage = usage;
          execution = completion;
          pending = charge(usage, candidate.offering.price) > candidate.cost;
          break;
        } catch (e) {
          const definitive =
            e instanceof ProviderError && e.definitive && !started;
          await this.store.attemptResult(
            principal,
            request.id,
            attempt,
            definitive ? "failed" : "unknown",
            usage,
            usage
              ? charge(usage, candidate.offering.price).toString()
              : definitive
                ? "0"
                : null,
            upstream,
          );
          if (!definitive) {
            pending = true;
            execution = started ? "partial" : "unknown";
            break;
          }
          if (!(e instanceof ProviderError && e.retryable)) break;
        }
      }
      await this.store.finish(
        principal,
        request.id,
        execution,
        finalUsage,
        pending,
      );
      const settled = await this.store.get(principal, request.id);
      // Only a finished response has whole tool calls; a cut-off one must never look callable.
      const completedCalls =
        execution === "succeeded"
          ? [...toolCalls.entries()]
              .sort(([a], [b]) => a - b)
              .map(([index, call]) => ({
                index,
                id: call.id,
                name: call.name,
                arguments: call.arguments.trim() || "{}",
              }))
          : [];
      for (const call of completedCalls)
        await send("output.tool_call.completed", call);
      const finishReason =
        execution === "partial" && finalUsage
          ? "length"
          : execution !== "succeeded"
            ? undefined
            : completedCalls.length
              ? "tool_calls"
              : "stop";
      const usageReport = finalUsage
        ? { status: "confirmed", meters: finalUsage }
        : {
            status: settled.billing === "released" ? "not_billable" : "pending",
          };
      await send(
        execution === "succeeded" ? "request.completed" : "request.failed",
        {
          execution_status: execution,
          billing_status: settled.billing,
          ...(finishReason ? { finish_reason: finishReason } : {}),
          usage: usageReport,
        },
      );
      return {
        id: request.id,
        execution_status: execution,
        billing_status: settled.billing,
        service_version: service.version,
        model: lastModel,
        tier: service.tier,
        output: {
          role: "assistant",
          content: text,
          ...(completedCalls.length
            ? {
                tool_calls: completedCalls.map(({ index: _, ...call }) => call),
              }
            : {}),
        },
        finish_reason: finishReason,
        usage: usageReport,
        billing: {
          charges:
            finalUsage && settled.billing === "settled"
              ? legs
                  .filter(
                    (l) =>
                      l.kind === "customer_sale" ||
                      principal.scopes.includes("billing:read"),
                  )
                  .map((l) => ({
                    kind: l.kind,
                    asset: l.price.asset,
                    atoms: charge(finalUsage!, l.price).toString(),
                    scale: 9,
                  }))
              : [],
        },
      };
    } catch (e) {
      // Unexpected storage/transport failure must never turn an uncertain dispatch into a refund.
      await this.store
        .finish(
          principal,
          request.id,
          started ? "partial" : "unknown",
          null,
          true,
        )
        .catch(() => {});
      const failure =
        e instanceof RouterError
          ? e
          : new RouterError("execution_unknown", 503, false);
      failure.request_id = request.id;
      throw failure;
    } finally {
      clearTimeout(timeout);
      clearInterval(poll);
      signal?.removeEventListener("abort", abort);
    }
  }
}
export class MockProvider implements Provider {
  calls = 0;
  constructor(
    private behavior: "success" | "reject" | "unknown" | "partial" = "success",
    private delay = 0,
  ) {}
  async *invoke(input: Invocation, _offering: Offering, signal: AbortSignal) {
    this.calls++;
    if (input.input.task)
      throw new ProviderError(true, false, "unsupported_task");
    if (this.delay)
      await new Promise<void>((resolve, reject) => {
        const t = setTimeout(resolve, this.delay);
        signal.addEventListener(
          "abort",
          () => {
            clearTimeout(t);
            reject(new ProviderError(false, false));
          },
          { once: true },
        );
      });
    if (this.behavior === "reject")
      throw new ProviderError(true, true, "rate_limited");
    if (this.behavior === "unknown" || signal.aborted)
      throw new ProviderError(false, false);
    yield { type: "delta" as const, text: "Hello from SuperRouter." };
    if (this.behavior === "partial") throw new ProviderError(false, false);
    yield {
      type: "usage" as const,
      usage: {
        input: 10,
        cached: 0,
        cache_write: 0,
        output: Math.min(5, input.limits.max_output_tokens),
      },
      upstream_id: randomUUID(),
    };
    yield { type: "done" as const };
  }
}

/**
 * A scripted model for demos and tests that calls tools without a network. It calls the tool named by `tool_choice`
 * (or the first declared tool) with `args`, and answers in text once the last message is a tool result.
 */
export class MockToolProvider implements Provider {
  capabilities = { tools: true, images: true };
  calls = 0;
  constructor(
    private args: Record<string, unknown> = {},
    private behavior: "success" | "truncated" | "reject" = "success",
  ) {}
  async *invoke(input: Invocation, _offering: Offering, signal: AbortSignal) {
    this.calls++;
    if (this.behavior === "reject")
      throw new ProviderError(true, true, "rate_limited");
    if (signal.aborted) throw new ProviderError(false, false);
    const { messages, tools, tool_choice: choice } = input.input;
    const usage = { input: 10, cached: 0, cache_write: 0, output: 5 };
    const last = messages[messages.length - 1];
    if (!tools || choice === "none" || last?.role === "tool") {
      yield {
        type: "delta" as const,
        text:
          last?.role === "tool"
            ? "The tool finished."
            : "Hello from SuperRouter.",
      };
    } else {
      const name = typeof choice === "object" ? choice.name : tools[0]!.name;
      const json = JSON.stringify(this.args);
      const middle = Math.ceil(json.length / 2);
      const id = "call_" + randomUUID().replaceAll("-", "");
      yield {
        type: "tool_call" as const,
        index: 0,
        id,
        name,
        arguments_delta: json.slice(0, middle),
      };
      if (this.behavior === "truncated") {
        yield { type: "usage" as const, usage, upstream_id: randomUUID() };
        yield { type: "done" as const, completion: "partial" as const };
        return;
      }
      yield {
        type: "tool_call" as const,
        index: 0,
        arguments_delta: json.slice(middle),
      };
    }
    yield { type: "usage" as const, usage, upstream_id: randomUUID() };
    yield { type: "done" as const };
  }
}

/** Simulated bounded task adapter. Produces a receipt, never actual image/video/audio. */
export class MockTaskProvider implements Provider {
  calls = 0;
  constructor(
    private behavior: "success" | "reject" | "unknown" | "partial" = "success",
  ) {}
  async *invoke(input: Invocation, offering: Offering, signal: AbortSignal) {
    this.calls++;
    const task = input.input.task;
    if (!task || task.kind !== offering.task_kind || this.behavior === "reject")
      throw new ProviderError(true, false, "unsupported_task");
    if (signal.aborted || this.behavior === "unknown")
      throw new ProviderError(false, false, "usage_unknown");
    const meter = kindMeter[task.kind];
    const units = input.limits.max_usage?.[meter] ?? 0;
    if (!units || units > (offering.max_usage?.[meter] ?? 0))
      throw new ProviderError(true, false, "meter_limit");
    const partial = this.behavior === "partial";
    const completed = partial
      ? meter === "tasks"
        ? 0
        : Math.floor(units / 2)
      : units;
    yield {
      type: "delta" as const,
      text: `Simulated ${task.kind} task receipt; no media generated.`,
    };
    yield {
      type: "usage" as const,
      usage: {
        input: 0,
        cached: 0,
        cache_write: 0,
        output: 0,
        [meter]: completed,
      },
      upstream_id: randomUUID(),
    };
    yield {
      type: "done" as const,
      completion: partial ? ("partial" as const) : ("succeeded" as const),
    };
  }
}
export * from "./credit-rates.js";
