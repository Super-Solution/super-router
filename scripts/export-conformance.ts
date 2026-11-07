// Writes conformance/pricing.v1.json: language-neutral cases generated from the TypeScript
// reference (charge, bound, compileCreditPrice). Go and Python must reproduce every case exactly;
// tests/conformance.test.ts fails if this file drifts from the TypeScript behaviour.
import { writeFileSync } from "node:fs";
import {
  bound,
  charge,
  compileCreditPrice,
} from "@supersolutions/super-router-core";
import { RouterError } from "@supersolutions/super-router-contracts";

let seed = 20261107;
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

export function run<T>(fn: () => T): { ok: unknown } | { error: string } {
  try {
    const value = fn();
    return { ok: typeof value === "bigint" ? value.toString() : value };
  } catch (e) {
    if (e instanceof RouterError) return { error: e.code };
    throw e;
  }
}

function price(media: boolean) {
  const per = pick([1, 2, 3, 7, 1000, 1_000_000, 999_999_937, 1_000_000_000]);
  const p: Record<string, unknown> = {
    asset: "credits",
    input: pick(["0", big(1 + rand(12)), big(25)]),
    cached: pick(["0", big(1 + rand(10))]),
    cache_write: pick(["0", big(1 + rand(10))]),
    output: pick(["0", big(1 + rand(14))]),
    per,
  };
  if (media) {
    const m: Record<string, unknown> = {};
    for (const k of ["images", "video_ms", "audio_ms", "tasks"]) {
      if (rand(2))
        m[k] = {
          atoms: pick(["0", big(1 + rand(12))]),
          per: pick([1, 3, 1000, 1_000_000_000]),
        };
    }
    if (Object.keys(m).length) p.media = m;
  }
  return p;
}
function usage(media: boolean) {
  const u: Record<string, number> = {
    input: pick([0, 1, rand(100000), Number.MAX_SAFE_INTEGER]),
    cached: pick([0, rand(5000)]),
    cache_write: pick([0, rand(5000)]),
    output: pick([0, 1, rand(100000)]),
  };
  if (media)
    for (const k of ["images", "video_ms", "audio_ms", "tasks"])
      if (rand(3) === 0) u[k] = pick([0, 1, rand(100000)]);
  return u;
}

const chargeCases = [];
for (let i = 0; i < 160; i++) {
  const media = i % 2 === 1;
  const p = price(media),
    u = usage(media);
  chargeCases.push({
    price: p,
    usage: u,
    expect: run(() => charge(u as never, p as never)),
  });
}
const boundCases = [];
for (let i = 0; i < 80; i++) {
  const p = price(false);
  const input = pick([0, 1, rand(200000)]),
    output = pick([1, rand(8192) + 1]);
  boundCases.push({
    price: p,
    input,
    output,
    expect: run(() => bound(input, output, p as never)),
  });
}
const ratio = (units: string, credits = "1") => ({ units, credits });
const compileConfigs: unknown[] = [
  {
    asset: "credits",
    tokens: {
      input: ratio("1000"),
      cached: ratio("10000"),
      cache_write: ratio("800"),
      output: ratio("250"),
    },
  },
  {
    asset: "credits",
    tokens: {
      input: ratio("3"),
      cached: ratio("7"),
      cache_write: ratio("11"),
      output: ratio("13"),
    },
  },
  {
    asset: "credits",
    tokens: {
      input: ratio("1000000", "5"),
      cached: ratio("1000000", "1"),
      cache_write: ratio("1000000", "6"),
      output: ratio("1000000", "25"),
    },
  },
  { asset: "credits", media: { images: ratio("1", "2") } },
  {
    asset: "credits",
    media: { video_ms: ratio("1000", "3"), audio_ms: ratio("500") },
  },
  {
    asset: "credits",
    tokens: {
      input: ratio("1"),
      cached: ratio("1"),
      cache_write: ratio("1"),
      output: ratio("1"),
    },
    media: { tasks: ratio("1", "40") },
  },
  {
    asset: "credits",
    tokens: {
      input: ratio("999999937"),
      cached: ratio("2"),
      cache_write: ratio("2"),
      output: ratio("2"),
    },
  },
  // precision limits: the least common denominator exceeds 1e9
  {
    asset: "credits",
    tokens: {
      input: ratio("999999937"),
      cached: ratio("999999929"),
      cache_write: ratio("1"),
      output: ratio("1"),
    },
  },
  { asset: "credits", media: { images: ratio("1000000001") } },
  // invalid shapes
  { asset: "credits" },
  {
    asset: "credits",
    tokens: { input: ratio("1"), cached: ratio("1"), cache_write: ratio("1") },
  },
  {
    asset: "credits",
    tokens: {
      input: ratio("0"),
      cached: ratio("1"),
      cache_write: ratio("1"),
      output: ratio("1"),
    },
  },
  {
    asset: "credits",
    tokens: {
      input: ratio("01"),
      cached: ratio("1"),
      cache_write: ratio("1"),
      output: ratio("1"),
    },
  },
  {
    asset: "credits",
    tokens: {
      input: ratio("1"),
      cached: ratio("1"),
      cache_write: ratio("1"),
      output: ratio("1"),
    },
    extra: true,
  },
  {
    asset: "",
    tokens: {
      input: ratio("1"),
      cached: ratio("1"),
      cache_write: ratio("1"),
      output: ratio("1"),
    },
  },
  {
    asset: "bad asset",
    tokens: {
      input: ratio("1"),
      cached: ratio("1"),
      cache_write: ratio("1"),
      output: ratio("1"),
    },
  },
  { asset: "credits", media: {} },
  {
    asset: "credits",
    tokens: {
      input: { units: "1", credits: "1", more: 1 },
      cached: ratio("1"),
      cache_write: ratio("1"),
      output: ratio("1"),
    },
  },
  {
    asset: "credits",
    tokens: {
      input: ratio("1".padEnd(31, "0")),
      cached: ratio("1"),
      cache_write: ratio("1"),
      output: ratio("1"),
    },
  },
  {
    asset: "credits",
    tokens: {
      input: { units: 1000, credits: "1" },
      cached: ratio("1"),
      cache_write: ratio("1"),
      output: ratio("1"),
    },
  },
  "not an object",
  null,
];
for (let i = 0; i < 40; i++) {
  const friendly = [
    "1",
    "2",
    "3",
    "4",
    "5",
    "6",
    "8",
    "10",
    "12",
    "20",
    "25",
    "50",
    "100",
    "125",
    "1000",
    "10000",
    "1000000",
  ];
  const r = () =>
    ratio(
      i % 8 === 7 ? big(1 + rand(9)) : pick(friendly),
      pick(["1", "1", big(1 + rand(3))]),
    );
  compileConfigs.push({
    asset: "a:b/c-d_1.2",
    tokens: { input: r(), cached: r(), cache_write: r(), output: r() },
  });
}
const compileCases = compileConfigs.map((config) => ({
  config,
  expect: run(() => compileCreditPrice(config)),
}));

const out = {
  version: 1,
  note: "Generated by scripts/export-conformance.ts from the TypeScript reference. Atoms are decimal strings; totals are decimal strings.",
  charge: chargeCases,
  bound: boundCases,
  compile_credit_price: compileCases,
};
writeFileSync(
  new URL("../conformance/pricing.v1.json", import.meta.url),
  JSON.stringify(out, null, 1) + "\n",
);
const errors = [...chargeCases, ...boundCases, ...compileCases].filter(
  (c) => "error" in c.expect,
).length;
console.log(
  `charge ${chargeCases.length}, bound ${boundCases.length}, compile ${compileCases.length}; ${errors} error cases`,
);
