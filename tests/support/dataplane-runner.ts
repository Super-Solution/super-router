// Runs conformance/dataplane.v1.json against any DataStore. The same file drives the Go and Python ports.
import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import {
  MockProvider,
  Router,
  type DataStore,
  type Leg,
} from "@supersolutions/super-router-core";
import {
  Invocation,
  Service,
  RouterError,
  type Principal,
} from "@supersolutions/super-router-contracts";

type Json = Record<string, any>;
export interface Harness {
  store: DataStore;
  /** Epoch ms now. In-memory stores use a virtual clock, SQL stores the real one. */
  clock: () => number;
  advance: (ms: number) => Promise<void>;
  buckets: (
    p: Principal,
  ) => Promise<{ source: string; remaining: string; held: string }[]>;
}
export const digestKey = "conformance-digest-key-0123456789abcdef";

function matches(
  actual: unknown,
  expected: unknown,
  refs: Map<string, string>,
  path: string,
): void {
  if (typeof expected === "string") {
    if (expected.startsWith("$")) {
      assert.equal(actual, refs.get(expected.slice(1)), path);
    } else if (expected === ">0") {
      assert.ok(
        typeof actual === "string" && BigInt(actual) > 0n,
        `${path}: ${String(actual)} is not > 0`,
      );
    } else assert.equal(actual, expected, path);
    return;
  }
  if (Array.isArray(expected)) {
    assert.ok(Array.isArray(actual), `${path}: expected array`);
    assert.equal(actual.length, expected.length, `${path}: length`);
    expected.forEach((e, i) => matches(actual[i], e, refs, `${path}[${i}]`));
    return;
  }
  if (expected && typeof expected === "object") {
    assert.ok(actual && typeof actual === "object", `${path}: expected object`);
    for (const [k, v] of Object.entries(expected))
      matches((actual as Json)[k], v, refs, `${path}.${k}`);
    return;
  }
  assert.deepEqual(actual, expected, path);
}
function merge(base: Json, patch: Json): Json {
  const out: Json = { ...base };
  for (const [k, v] of Object.entries(patch))
    out[k] =
      v &&
      typeof v === "object" &&
      !Array.isArray(v) &&
      base[k] &&
      typeof base[k] === "object"
        ? merge(base[k], v)
        : v;
  return out;
}

export async function runScenario(
  data: Json,
  scenario: Json,
  make: () => Promise<Harness>,
): Promise<void> {
  const h = await make();
  const refs = new Map<string, string>();
  const providers = Object.fromEntries(
    Object.entries(scenario.providers ?? data.providers).map(
      ([name, behavior]) => [name, new MockProvider(behavior as any)],
    ),
  );
  const router = new Router(h.store, providers, digestKey, h.clock);
  const workspace = "ws-" + randomUUID();
  const principals: Record<string, Principal> = {};
  for (const [name, p] of Object.entries(data.principals as Json))
    principals[name] = { workspace, environment: "test", ...(p as any) };
  const who = (name?: string) => principals[name ?? "owner"]!;
  const service = (name: string, patch?: Json) =>
    merge(data.services[name], patch ?? {});
  const pending: string[] = [];

  for (const [index, step] of (scenario.steps as Json[]).entries()) {
    const label = `${scenario.name} #${index} ${step.op}`;
    let result: unknown;
    let failure: RouterError | undefined;
    try {
      switch (step.op) {
        case "publish":
          result = await h.store.publish(
            who(step.principal),
            service(step.service, step.patch),
            step.mode,
          );
          break;
        case "grant":
          result = await h.store.grant(
            who(step.principal),
            step.kind,
            step.asset,
            step.amount,
            step.source,
            step.expires_in_ms == null
              ? undefined
              : new Date(h.clock() + step.expires_in_ms),
          );
          break;
        case "infer": {
          const input = Invocation.parse(
            merge(data.invocation, step.input ?? {}),
          );
          result = await router.infer(
            who(step.principal),
            input,
            step.key ?? "key-" + index,
          );
          break;
        }
        case "balance":
          result = await h.store.balance(who(step.principal));
          break;
        case "buckets":
          result = await h.buckets(who(step.principal));
          break;
        case "get": {
          const r = await h.store.get(
            who(step.principal),
            refs.get(step.request)!,
          );
          result = {
            execution: r.execution,
            billing: r.billing,
            cancel_requested: r.cancel_requested,
            output_started: r.output_started,
            provider_spent: r.provider_spent,
          };
          break;
        }
        case "cancel":
          await h.store.cancel(who(step.principal), refs.get(step.request)!);
          break;
        case "advance":
          await h.advance(step.ms);
          break;
        case "disable":
          await h.store.disable(who(step.principal), step.offering);
          break;
        case "recover":
          result = await h.store.recover();
          break;
        case "provider_calls":
          result = (providers[step.provider] as MockProvider).calls;
          break;
        case "reserve": {
          const s = Service.parse(service(step.service, step.patch));
          const p = who(step.principal);
          const legs = (await h.store.accounts(p, s)).map((l: Leg) => ({
            ...l,
            reserved: step.reserved[l.kind] ?? "0",
          }));
          const out = await h.store.reserve(
            p,
            step.key,
            step.digest ?? "d",
            s,
            legs,
            new Date(h.clock() + step.timeout_ms).toISOString(),
          );
          refs.set(step.as, out.request.id);
          result = {
            fresh: out.fresh,
            execution: out.request.execution,
            billing: out.request.billing,
          };
          break;
        }
        case "begin": {
          const s = Service.parse(service(step.service, step.patch));
          const offering = s.offerings.find((o) => o.id === step.offering)!;
          const attempt = await h.store.begin(
            who(step.principal),
            refs.get(step.request)!,
            offering,
            step.bound,
          );
          refs.set(step.as, attempt);
          break;
        }
        case "attempt":
          await h.store.attemptResult(
            who(step.principal),
            refs.get(step.request)!,
            refs.get(step.attempt)!,
            step.status,
            step.usage ?? null,
            step.cost ?? null,
          );
          break;
        case "output":
          await h.store.output(who(step.principal), refs.get(step.request)!);
          break;
        case "finish":
          await h.store.finish(
            who(step.principal),
            refs.get(step.request)!,
            step.execution,
            step.usage ?? null,
            step.pending ?? false,
          );
          break;
        default:
          throw new Error(`unknown op ${step.op}`);
      }
    } catch (e) {
      if (!(e instanceof RouterError)) throw e;
      failure = e;
    }
    if (step.error) {
      assert.ok(failure, `${label}: expected error ${step.error.code}`);
      assert.equal(failure.code, step.error.code, label);
      if (step.error.status)
        assert.equal(failure.status, step.error.status, label);
      continue;
    }
    if (failure) assert.fail(`${label}: unexpected ${failure.code}`);
    if (step.as && step.op === "infer") refs.set(step.as, (result as Json).id);
    if (step.expect !== undefined) matches(result, step.expect, refs, label);
  }
  void pending;
}
