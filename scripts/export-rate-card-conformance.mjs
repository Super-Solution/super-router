// Writes the `rate_card_charge` and `rate_card_bound` cases of backends/rust/conformance/pricing.v1.json.
//
// The rate card (Rust and Go only, like custom billing units) is not in the TypeScript reference, so these cases are
// not generated from it. This script is the independent oracle instead: it states the rule once with plain BigInt
// arithmetic and no code from either backend, so a bug that Rust and Go share would still fail here.
//
//   node scripts/export-rate-card-conformance.mjs
//
// Rule: a rate that is not given is the rate of the kind it falls back to (cached and cache_write to input,
// cache_write_1h to cache_write, reasoning to output). charge = ceil(sum(count * rate) / per) + per_call, rounded up
// once. bound = ceil((max_input * dearest input rate + max_output * dearest output rate) / per) + per_call.
import { readFileSync, writeFileSync } from "node:fs";

const file = new URL("../backends/rust/conformance/pricing.v1.json", import.meta.url);

const ceilDiv = (n, d) => (n + d - 1n) / d;

function effective(card) {
  const input = BigInt(card.input);
  const output = BigInt(card.output);
  const cached = card.cached === undefined ? input : BigInt(card.cached);
  const cacheWrite = card.cache_write === undefined ? input : BigInt(card.cache_write);
  const cacheWrite1h = card.cache_write_1h === undefined ? cacheWrite : BigInt(card.cache_write_1h);
  const reasoning = card.reasoning === undefined ? output : BigInt(card.reasoning);
  const perCall = card.per_call === undefined ? 0n : BigInt(card.per_call);
  return { per: BigInt(card.per), input, cached, cacheWrite, cacheWrite1h, output, reasoning, perCall };
}

const KINDS = ["input", "cached", "cache_write", "cache_write_1h", "output", "reasoning"];
const rateOf = (e, kind) =>
  ({ input: e.input, cached: e.cached, cache_write: e.cacheWrite, cache_write_1h: e.cacheWrite1h, output: e.output, reasoning: e.reasoning })[kind];

// A token count test case is [card, usage, expectation]; errors are written down, not computed.
const charges = [];
const bounds = [];

function charge(card, usage) {
  const e = effective(card);
  const sum = KINDS.reduce((s, k) => s + BigInt(usage[k] ?? 0) * rateOf(e, k), 0n);
  charges.push({ card, usage, expect: { ok: (ceilDiv(sum, e.per) + e.perCall).toString() } });
}
function chargeError(card, usage, code) {
  charges.push({ card, usage, expect: { error: code } });
}
function bound(card, input, output) {
  const e = effective(card);
  const maxIn = [e.input, e.cached, e.cacheWrite, e.cacheWrite1h].reduce((a, b) => (a > b ? a : b));
  const maxOut = e.output > e.reasoning ? e.output : e.reasoning;
  bounds.push({ card, input, output, expect: { ok: (ceilDiv(BigInt(input) * maxIn + BigInt(output) * maxOut, e.per) + e.perCall).toString() } });
}
function boundError(card, input, output, code) {
  bounds.push({ card, input, output, expect: { error: code } });
}

// Rates per million tokens in nano-USD (0.30 / 2.50 USD per million), the way an application prices a model.
const perMillion = { per: 1000000, input: "300000000", cached: "30000000", output: "2500000000" };
charge(perMillion, { input: 70227, cached: 0, output: 1164 });
charge(perMillion, { input: 15236, cached: 8000, output: 44 });
charge(perMillion, {});

// All six kinds with rates of their own, and a fee.
const all = {
  per: 1000000,
  input: "3000000000",
  cached: "300000000",
  cache_write: "3750000000",
  cache_write_1h: "6000000000",
  output: "15000000000",
  reasoning: "18000000000",
  per_call: "1500",
};
charge(all, { input: 1200, cached: 90000, cache_write: 4000, cache_write_1h: 2500, output: 800, reasoning: 3100 });
charge(all, {});
charge(all, { reasoning: 1 });

// Fallbacks: left out means the rate of the kind it is priced as, and the chain resolves in order.
const bare = { per: 1000, input: "7", output: "11" };
charge(bare, { input: 100, cached: 100, cache_write: 100, cache_write_1h: 100, output: 100, reasoning: 100 });
charge({ ...bare, cache_write: "20" }, { cache_write: 10, cache_write_1h: 10 });
charge({ ...bare, cache_write: "20", cache_write_1h: "30" }, { cache_write: 10, cache_write_1h: 10 });
charge({ ...bare, cached: "0", reasoning: "0" }, { cached: 99999, reasoning: 99999, input: 1 });
charge({ ...bare, per_call: "0" }, { input: 1 });

// Rounding: one round up over the whole sum, not one per kind. Two tokens at 1 atom per 1000 cost 1 atom, not 2.
charge({ per: 1000, input: "1", output: "1" }, { input: 1, reasoning: 1 });
charge({ per: 1000, input: "1", output: "1", reasoning: "1", cached: "1" }, { cached: 1, reasoning: 1, output: 1, input: 1 });
charge({ per: 1000, input: "1", output: "1" }, { input: 1000 });
charge({ per: 1000, input: "1", output: "1" }, { input: 1001 });
charge({ per: 1, input: "1", output: "1", per_call: "5" }, { input: 0 });

// Big numbers: 30-digit rates and counts near the safe-integer limit.
const huge = "9".repeat(30);
charge({ per: 1000000000, input: huge, output: huge, reasoning: huge, per_call: huge }, { input: 9007199254740991, reasoning: 9007199254740991 });
charge({ per: 999999937, input: "7309638107", output: "1", cache_write_1h: "123456789012345678901234567890" }, { input: 1, cache_write_1h: 3 });

// Refusals.
chargeError({ per: 0, input: "1", output: "1" }, {}, "invalid_rate_card");
chargeError({ per: -1, input: "1", output: "1" }, {}, "invalid_rate_card");
chargeError({ per: 1000000001, input: "1", output: "1" }, {}, "invalid_rate_card");
for (const text of ["", "01", "-1", "1.5", "1e3", " 1", "9".repeat(31)]) chargeError({ per: 1, input: text, output: "1" }, {}, "invalid_rate_card");
chargeError({ per: 1, input: "1", output: "1", reasoning: "x" }, {}, "invalid_rate_card");
chargeError({ per: 1, input: "1", output: "1", per_call: "-5" }, {}, "invalid_rate_card");
chargeError({ per: 1, input: "1", output: "1", cache_write_1h: "" }, {}, "invalid_rate_card");
chargeError({ per: 1, input: "1", output: "1" }, { input: -1 }, "invalid_usage");
chargeError({ per: 1, input: "1", output: "1" }, { reasoning: -1 }, "invalid_usage");

// Bound: the dearest input kind and the dearer of output and reasoning, plus the fee.
bound(perMillion, 128000, 8192);
bound(all, 128000, 8192);
bound(all, 0, 0);
bound(bare, 1000, 1000);
bound({ ...bare, cache_write_1h: "99" }, 10, 10);
bound({ ...bare, reasoning: "99" }, 10, 10);
bound({ ...bare, cached: "99" }, 10, 10);
bound({ per: 1000, input: "1", output: "1" }, 1, 1);
bound({ per: 1000, input: "1", output: "1", per_call: "42" }, 0, 0);
bound({ per: 999999937, input: huge, output: huge, reasoning: huge, per_call: huge }, 9007199254740991, 9007199254740991);
boundError({ per: 0, input: "1", output: "1" }, 1, 1, "invalid_rate_card");
boundError({ per: 1, input: "01", output: "1" }, 1, 1, "invalid_rate_card");
boundError({ per: 1, input: "1", output: "1" }, -1, 1, "invalid_usage");
boundError({ per: 1, input: "1", output: "1" }, 1, -1, "invalid_usage");

const data = JSON.parse(readFileSync(file, "utf8"));
data.rate_card_charge = charges;
data.rate_card_bound = bounds;
writeFileSync(file, JSON.stringify(data, null, 1) + "\n");
console.log(`rate_card_charge: ${charges.length} cases, rate_card_bound: ${bounds.length} cases`);
