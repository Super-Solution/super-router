// Writes conformance/routing.v1.json: language-neutral cases generated from the TypeScript
// reference `route` (hard limits first, then weighted scoring, ordered by score then id).
// Go and Python must reproduce every case: same candidates in the same order, same exclusion
// reasons, same costs (decimal strings) and the same scores (IEEE doubles, compared exactly).
// tests/conformance.test.ts fails if this file drifts from the TypeScript behaviour.
//
// Offering ids use [a-z0-9] only, so the id tie-break (localeCompare in TypeScript) is the same
// as a byte-wise comparison in every other language.
import { writeFileSync } from "node:fs";
import { route } from "@supersolutions/super-router-core";
import { RouterError } from "@supersolutions/super-router-contracts";

let seed = 20261108;
const rand = (n: number) => {
  seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0;
  return seed % n;
};
const pick = <T>(items: T[]) => items[rand(items.length)]!;
const big = (digits: number) => {
  let s = String(1 + rand(9));
  for (let i = 1; i < digits; i++) s += String(rand(10));
  return s;
};
const chance = (n: number) => rand(n) === 0;

const kinds = ["image", "video", "audio", "task"] as const;
const meterOf = {
  image: "images",
  video: "video_ms",
  audio: "audio_ms",
  task: "tasks",
} as const;
const meters = ["images", "video_ms", "audio_ms", "tasks"] as const;
const regions = ["us", "eu", "tw", "jp"];
const labels = ["fast", "safe", "cheap", "eu-only", "beta"];
const models = ["m1", "m2", "m3"];

function price(kind: (typeof kinds)[number] | undefined, full: boolean) {
  const p: Record<string, unknown> = {
    asset: "credits",
    input: pick(["0", "1", big(1 + rand(6))]),
    cached: pick(["0", "1", big(1 + rand(5))]),
    cache_write: pick(["0", big(1 + rand(5))]),
    output: pick(["0", "1", big(1 + rand(7))]),
    per: pick([1, 1000, 1_000_000]),
  };
  if (kind) {
    const m: Record<string, unknown> = {};
    for (const k of meters)
      if (k === meterOf[kind] ? !chance(12) : full && chance(3))
        m[k] = {
          atoms: pick(["0", "1", big(1 + rand(7))]),
          per: pick([1, 1000, 1_000_000]),
        };
    if (Object.keys(m).length) p.media = m;
  }
  return p;
}
function maxUsage(only: string | undefined, extras: boolean) {
  const u: Record<string, number> = {};
  for (const k of meters)
    if (k === only ? !chance(10) : extras && chance(4))
      u[k] = pick([0, 1, 5, 1000, 123456]);
  return u;
}

function offering(
  id: string,
  kind: (typeof kinds)[number] | undefined,
  svc: { regions: string[]; model: string },
  loose: boolean,
  tame: boolean,
) {
  const o: Record<string, unknown> = {
    id,
    provider: pick(["p1", "p2"]),
    model: loose ? pick(models) : svc.model,
    upstream_model: "u-" + id,
    region: loose ? pick(regions) : pick(svc.regions),
    labels: tame ? [...labels] : labels.filter(() => chance(2)),
    enabled: tame ? !chance(10) : !chance(8),
    source: pick(["platform", "byok"]),
    max_input_tokens: pick([100, 4096, 128000, 1000000]),
    max_output_tokens: tame ? 8192 : pick([64, 1024, 4096, 8192]),
    price: price(kind, true),
  };
  if (kind) {
    o.task_kind = kind;
    if (!chance(15)) o.max_usage = maxUsage(meterOf[kind], true);
  }
  if (!chance(4)) o.latency_ms = pick([0, 50, 400, 2500, 90000]);
  if (!chance(4)) o.reliability = pick([0, 0.5, 0.999, 1, rand(1000) / 1000]);
  if (!chance(4)) o.quality = pick([0, 0.25, 1, rand(1000) / 1000]);
  return o;
}

function weights() {
  const w = [rand(10), rand(10), rand(10), rand(10)];
  if (!w.some(Boolean)) w[0] = 1;
  const s = w.reduce((a, b) => a + b, 0);
  return {
    cost: w[0]! / s,
    latency: w[1]! / s,
    reliability: w[2]! / s,
    quality: w[3]! / s,
  };
}

function run(fn: () => unknown): { ok: unknown } | { error: string } {
  try {
    return { ok: fn() };
  } catch (e) {
    if (e instanceof RouterError) return { error: e.code };
    throw e;
  }
}
const view = (r: ReturnType<typeof route>) => ({
  candidates: r.candidates.map((c) => ({
    id: c.offering.id,
    cost: c.cost.toString(),
    score: c.score,
  })),
  excluded: r.excluded,
});

const cases = [];
for (let n = 0; n < 400; n++) {
  const kind = n % 3 === 0 ? undefined : pick([...kinds]);
  const tame = !chance(3);
  const loose = !tame && chance(2);
  const svcRegions = regions.filter(() => chance(2));
  if (!svcRegions.length) svcRegions.push(pick(regions));
  const model = pick(models);
  const mode = pick(["specific", "pool", "tier"] as const);
  const count = 1 + rand(8);
  const ids = new Set<string>();
  while (ids.size < count) ids.add(pick(["a", "b", "c", "x", "m"]) + rand(40));
  const svc: Record<string, unknown> = {
    id: "svc" + n,
    version: 1,
    mode,
    offerings: [...ids].map((id) =>
      // a task service sometimes lists an offering of another kind or a plain text one
      offering(
        id,
        kind && (tame || !chance(6))
          ? kind
          : chance(6)
            ? pick([...kinds])
            : kind,
        { regions: svcRegions, model },
        loose,
        tame,
      ),
    ),
    regions: svcRegions,
    required_labels: tame ? [] : labels.filter(() => chance(6)),
    price: price(undefined, false),
    max_attempts: 2,
    max_output_tokens: 4096,
    max_provider_cost: tame
      ? pick([big(15), big(25), "1" + "0".repeat(29)])
      : pick(["0", "50", big(1 + rand(4)), big(12), big(25)]),
    weights: weights(),
    cost_scale: pick(["1", "7", "1000", big(1 + rand(12)), big(20)]),
    latency_scale_ms: pick([1, 1000, 5000.5, 100000]),
    unknown_score: pick([0, 0.5, 1, rand(1000) / 1000]),
  };
  if (mode === "specific") svc.model = model;
  if (mode === "tier") svc.tier = "pro";
  if (kind && (tame || !chance(10))) svc.task_kind = kind;
  const invocation: Record<string, unknown> = {
    input: kind
      ? { messages: [], task: { kind, prompt: "p" } }
      : { messages: [{ role: "user", content: "hi" }] },
    limits: {
      max_output_tokens: tame ? pick([1, 64]) : pick([1, 64, 1024, 4096, 8192]),
      timeout_ms: 30000,
    },
    stream: false,
  };
  if (kind)
    (invocation.limits as Record<string, unknown>).max_usage = chance(15)
      ? undefined
      : maxUsage(meterOf[kind], chance(tame ? 25 : 4));
  const input = JSON.parse(JSON.stringify(invocation));
  const service = JSON.parse(JSON.stringify(svc));
  cases.push({
    service,
    invocation: input,
    expect: run(() => view(route(service as never, input as never))),
  });
}

const out = {
  version: 1,
  note: "Generated by scripts/export-routing-conformance.ts from the TypeScript reference. Costs are decimal strings; scores are IEEE doubles and must match exactly.",
  route: cases,
};
writeFileSync(
  new URL("../conformance/routing.v1.json", import.meta.url),
  '{\n "version": 1,\n "note": ' +
    JSON.stringify(out.note) +
    ',\n "route": [\n' +
    cases.map((c) => "  " + JSON.stringify(c)).join(",\n") +
    "\n ]\n}\n",
);
const withCandidates = cases.filter(
  (c) =>
    "ok" in c.expect &&
    (c.expect.ok as { candidates: unknown[] }).candidates.length,
).length;
const reasons = new Set<string>();
for (const c of cases)
  if ("ok" in c.expect)
    for (const e of (c.expect.ok as { excluded: { reason: string }[] })
      .excluded)
      reasons.add(e.reason);
const errors = cases.filter((c) => "error" in c.expect).length;
console.log(
  `route ${cases.length}: ${withCandidates} with candidates, ${errors} error cases; reasons ${[...reasons].sort().join(",")}`,
);
