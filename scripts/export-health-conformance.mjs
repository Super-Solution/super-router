// Writes backends/rust/conformance/health.v1.json: the cooldown book, as scripted incidents.
//
// A case is a sequence of steps against one book: `admit` (what the caller is told), `fail` (an attempt failed with a
// class) and `succeed`. The expected answers come from the small model below, written here in plain JavaScript with
// no code from either backend; Rust and Go replay the same steps and must give the same answers. There is no
// TypeScript reference for this (Rust and Go only).
//
//   node scripts/export-health-conformance.mjs

import { writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const file = join(dirname(fileURLToPath(import.meta.url)), "..", "backends", "rust", "conformance", "health.v1.json");

const defaults = {
  rate_limited_ms: 5000,
  quota_exhausted_ms: 300000,
  credentials_rejected_ms: 300000,
  model_unavailable_ms: 600000,
  region_blocked_ms: 600000,
  max_ms: 1800000,
  probe_lease_ms: 30000,
};
const scoped = ["rate_limited", "quota_exhausted", "credentials_rejected", "model_unavailable", "region_blocked"];

function model(policy) {
  const p = { ...defaults, ...policy };
  const records = new Map();
  return {
    admit(id, at) {
      const r = records.get(id);
      if (!r) return { result: "open" };
      if (at < r.until) return { result: "cooling", until_ms: r.until, class: r.class };
      if (r.probe !== null && at < r.probe) return { result: "cooling", until_ms: r.probe, class: r.class };
      r.probe = at + p.probe_lease_ms;
      return { result: "probe" };
    },
    fail(id, cls, at) {
      if (!scoped.includes(cls)) return;
      const base = p[`${cls}_ms`];
      const r = records.get(id);
      if (r && at < r.until) return;
      const streak = r ? r.streak + 1 : 1;
      const length = Math.min(base * 2 ** (streak - 1), Math.max(p.max_ms, base));
      records.set(id, { class: cls, streak, until: at + length, probe: null });
    },
    succeed(id) {
      records.delete(id);
    },
    size: () => records.size,
  };
}

const A = (id, at) => ({ op: "admit", id, at });
const F = (id, at, cls) => ({ op: "fail", id, at, class: cls });
const S = (id, at) => ({ op: "succeed", id, at });

const scenarios = [
  ["an unknown offering is open", {}, [A("a", 0)]],
  ["a rejected key cools the offering for five minutes", {}, [F("a", 1000, "credentials_rejected"), A("a", 1001), A("a", 300999), A("a", 301000)]],
  ["a rate limit clears in seconds", {}, [F("a", 0, "rate_limited"), A("a", 4999), A("a", 5000)]],
  ["each class has its own length", {}, [
    F("q", 0, "quota_exhausted"), F("m", 0, "model_unavailable"), F("r", 0, "region_blocked"),
    A("q", 299999), A("q", 300000), A("m", 599999), A("m", 600000), A("r", 599999), A("r", 600000),
  ]],
  ["a refused request and an unknown outcome cool nothing", {}, [F("a", 0, "request_rejected"), F("a", 1, "unknown"), A("a", 2)]],
  ["after the cooldown only one caller probes", {}, [
    F("a", 0, "rate_limited"), A("a", 5000), A("a", 5001), A("a", 20000), A("a", 34999), A("a", 35000),
  ]],
  ["a successful probe clears the record", {}, [F("a", 0, "rate_limited"), A("a", 5000), S("a", 5100), A("a", 5101), A("a", 5102)]],
  ["a failed probe cools twice as long", {}, [
    F("a", 0, "rate_limited"), A("a", 5000), F("a", 5100, "rate_limited"), A("a", 5101), A("a", 15099), A("a", 15100),
  ]],
  ["the cooldown keeps doubling up to the ceiling", {}, [
    F("a", 0, "credentials_rejected"), A("a", 300000), F("a", 300000, "credentials_rejected"),
    A("a", 900000), F("a", 900000, "credentials_rejected"),
    A("a", 2699999), A("a", 2700000), F("a", 2700000, "credentials_rejected"), A("a", 4499999), A("a", 4500000),
  ]],
  ["a failure while already cooling is ignored", {}, [
    F("a", 0, "credentials_rejected"), F("a", 100, "credentials_rejected"), F("a", 200000, "quota_exhausted"),
    A("a", 299999), A("a", 300000), A("a", 300001),
  ]],
  ["a success from an in-flight request clears a cooldown", {}, [F("a", 0, "rate_limited"), S("a", 10), A("a", 11)]],
  ["offerings are independent", {}, [F("a", 0, "credentials_rejected"), A("b", 1), A("a", 1), S("b", 2), A("a", 3)]],
  ["a custom policy changes lengths, lease and ceiling", { rate_limited_ms: 100, max_ms: 250, probe_lease_ms: 10 }, [
    F("a", 0, "rate_limited"), A("a", 99), A("a", 100), A("a", 109), A("a", 110),
    F("a", 110, "rate_limited"), A("a", 309), A("a", 310), F("a", 310, "rate_limited"), A("a", 559), A("a", 560),
  ]],
  ["a ceiling below a class's base never shortens it", { max_ms: 1000 }, [F("a", 0, "credentials_rejected"), A("a", 299999), A("a", 300000)]],
  ["a very long streak stays at the ceiling", {}, Array.from({ length: 40 }, (_, i) => i).flatMap((i) => [F("a", i * 2000000, "rate_limited"), A("a", i * 2000000 + 1)])],
];

const cases = scenarios.map(([name, policy, steps]) => {
  const m = model(policy);
  const out = steps.map((s) => {
    if (s.op === "admit") return { ...s, expect: m.admit(s.id, s.at) };
    if (s.op === "fail") m.fail(s.id, s.class, s.at);
    else m.succeed(s.id);
    return s;
  });
  return { name, ...(Object.keys(policy).length ? { policy } : {}), steps: out, final_size: m.size() };
});

const doc = {
  version: 1,
  description: "Offering health book: scripted incidents of admit / fail / succeed with the expected answers. Written by scripts/export-health-conformance.mjs; Rust and Go only.",
  cases,
};
writeFileSync(file, JSON.stringify(doc, null, 1) + "\n");
console.log(`health: ${cases.length} cases, ${cases.reduce((n, c) => n + c.steps.length, 0)} steps`);
