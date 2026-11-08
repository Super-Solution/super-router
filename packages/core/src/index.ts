import { createHmac, randomUUID } from "node:crypto";
import {
  Invocation,
  Meters,
  Service,
  RouterError,
  ProviderError,
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
export function canonical(value: unknown): string {
  if (Array.isArray(value)) return "[" + value.map(canonical).join(",") + "]";
  if (value && typeof value === "object")
    return (
      "{" +
      Object.entries(value)
        .sort(([a], [b]) => a.localeCompare(b))
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
    const maxInput = Math.max(
      ...decision.candidates.map((c) => c.offering.max_input_tokens),
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
        candidates: decision.candidates.map((c) => ({
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
      lastModel: string | undefined;
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
      for (const candidate of decision.candidates.slice(
        0,
        service.max_attempts,
      )) {
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
              if (Buffer.byteLength(text) > 1_000_000)
                throw new ProviderError(false, false, "output_too_large");
              await send("output.delta", { text: event.text });
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
      await send(
        execution === "succeeded" ? "request.completed" : "request.failed",
        { execution_status: execution, billing_status: settled.billing },
      );
      return {
        id: request.id,
        execution_status: execution,
        billing_status: settled.billing,
        service_version: service.version,
        model: lastModel,
        tier: service.tier,
        output: { role: "assistant", content: text },
        usage: finalUsage
          ? { status: "confirmed", meters: finalUsage }
          : {
              status:
                settled.billing === "released" ? "not_billable" : "pending",
            },
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
