// Writes the `rate_card_charge`, `rate_card_bound`, `rate_schedule_charge`, `rate_schedule_bound`, `rate_card_defaulted`,
// `rate_tiers_charge` and `rate_tiers_bound`
// cases of backends/rust/conformance/pricing.v1.json.
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

// Schedule: the card in effect is the last step whose from_ms is not after the instant; before the first step there
// is none. A hold for a call that spans instants is the dearest bound of the card at the start and every card that
// starts inside (start, end]. A schedule needs a step, strictly increasing from_ms and valid cards.
const scheduleCharges = [];
const scheduleBounds = [];
const cardAt = (schedule, t) => schedule.steps.reduce((found, s) => (s.from_ms <= t ? s.card : found), null);
const step = (from_ms, card) => ({ from_ms, card });
const doubling = { steps: [step(0, { per: 1000, input: "10", output: "20" }), step(1000, { per: 1000, input: "20", output: "40", per_call: "3" }), step(5000, { per: 1000, input: "5", output: "10", cached: "1" })] };
function scheduleCharge(schedule, at_ms, usage) {
  const card = cardAt(schedule, at_ms);
  if (card === null) return void scheduleCharges.push({ schedule, at_ms, usage, expect: { error: "rate_card_not_in_effect" } });
  const e = effective(card);
  const sum = KINDS.reduce((acc, k) => acc + BigInt(usage[k] ?? 0) * rateOf(e, k), 0n);
  scheduleCharges.push({ schedule, at_ms, usage, expect: { ok: (ceilDiv(sum, e.per) + e.perCall).toString() } });
}
function scheduleBound(schedule, start_ms, end_ms, input, output) {
  const cards = [cardAt(schedule, start_ms), ...schedule.steps.filter((s) => s.from_ms > start_ms && s.from_ms <= end_ms).map((s) => s.card)];
  if (end_ms < start_ms) return void scheduleBounds.push({ schedule, start_ms, end_ms, input, output, expect: { error: "invalid_usage" } });
  if (cards[0] === null) return void scheduleBounds.push({ schedule, start_ms, end_ms, input, output, expect: { error: "rate_card_not_in_effect" } });
  const holds = cards.map((card) => {
    const e = effective(card);
    const maxIn = [e.input, e.cached, e.cacheWrite, e.cacheWrite1h].reduce((a, b) => (a > b ? a : b));
    const maxOut = e.output > e.reasoning ? e.output : e.reasoning;
    return ceilDiv(BigInt(input) * maxIn + BigInt(output) * maxOut, e.per) + e.perCall;
  });
  scheduleBounds.push({ schedule, start_ms, end_ms, input, output, expect: { ok: holds.reduce((a, b) => (a > b ? a : b)).toString() } });
}
const used = { input: 70, output: 30, cached: 40 };
for (const t of [0, 999, 1000, 4999, 5000, 9007199254740991]) scheduleCharge(doubling, t, used);
scheduleCharge(doubling, -1, used);
scheduleCharge({ steps: [step(500, { per: 1, input: "1", output: "1" })] }, 499, {});
scheduleCharge({ steps: [step(500, { per: 1, input: "1", output: "1" })] }, 500, { input: 2, output: 3 });
for (const [a, b] of [[0, 999], [1000, 4999], [900, 1100], [4000, 6000], [5000, 6000], [0, 9007199254740991], [7, 7]]) scheduleBound(doubling, a, b, 100, 50);
scheduleBound(doubling, 4000, 6000, 0, 0);
scheduleBound(doubling, -5, 0, 1, 1);
scheduleBound(doubling, 10, 9, 1, 1);
const scheduleErrors = [
  { steps: [] },
  { steps: [step(5, { per: 1, input: "1", output: "1" }), step(5, { per: 1, input: "2", output: "2" })] },
  { steps: [step(5, { per: 1, input: "1", output: "1" }), step(4, { per: 1, input: "2", output: "2" })] },
  { steps: [step(0, { per: 0, input: "1", output: "1" })] },
  { steps: [step(0, { per: 1, input: "01", output: "1" })] },
];
for (const schedule of scheduleErrors) scheduleCharges.push({ schedule, at_ms: 10, usage: {}, expect: { error: "invalid_rate_schedule" } });

// Tiers: the whole call is priced by the last tier whose `above` is less than the prompt (input + cached +
// cache_write + cache_write_1h), the first tier for an empty prompt. A hold is the dearest bound of the first tier and
// every tier with `above` less than the largest prompt. Tiers need a first `above` of 0, strictly increasing `above`
// and valid cards.
const tierCharges = [];
const tierBounds = [];
const tier = (above, card) => ({ above, card });
const promptOf = (usage) => ["input", "cached", "cache_write", "cache_write_1h"].reduce((n, k) => n + BigInt(usage[k] ?? 0), 0n);
const tierCardFor = (tiers, prompt) => tiers.tiers.reduce((found, t, i) => (i === 0 || BigInt(t.above) < prompt ? t.card : found), tiers.tiers[0].card);
function tierCharge(tiers, usage) {
  const negative = KINDS.some((k) => (usage[k] ?? 0) < 0);
  if (negative) return void tierCharges.push({ tiers, usage, expect: { error: "invalid_usage" } });
  const e = effective(tierCardFor(tiers, promptOf(usage)));
  const sum = KINDS.reduce((acc, k) => acc + BigInt(usage[k] ?? 0) * rateOf(e, k), 0n);
  tierCharges.push({ tiers, usage, expect: { ok: (ceilDiv(sum, e.per) + e.perCall).toString() } });
}
function tierBound(tiers, input, output) {
  if (input < 0 || output < 0) return void tierBounds.push({ tiers, input, output, expect: { error: "invalid_usage" } });
  const cards = [tiers.tiers[0].card, ...tiers.tiers.slice(1).filter((t) => BigInt(t.above) < BigInt(input)).map((t) => t.card)];
  const holds = cards.map((card) => {
    const e = effective(card);
    const maxIn = [e.input, e.cached, e.cacheWrite, e.cacheWrite1h].reduce((a, b) => (a > b ? a : b));
    const maxOut = e.output > e.reasoning ? e.output : e.reasoning;
    return ceilDiv(BigInt(input) * maxIn + BigInt(output) * maxOut, e.per) + e.perCall;
  });
  tierBounds.push({ tiers, input, output, expect: { ok: holds.reduce((a, b) => (a > b ? a : b)).toString() } });
}
const longContext = {
  tiers: [
    tier(0, { per: 1000000, input: "1250000", output: "10000000", cached: "125000" }),
    tier(200000, { per: 1000000, input: "2500000", output: "15000000", cached: "250000", per_call: "7" }),
    tier(1000000, { per: 1000000, input: "5000000", output: "30000000", cached: "500000", reasoning: "40000000" }),
  ],
};
for (const usage of [
  {},
  { input: 1, output: 1 },
  { input: 150000, cached: 50000, output: 10000 },
  { input: 150001, cached: 50000, output: 10000 },
  { input: 200000 },
  { input: 200001 },
  { cached: 200001 },
  { cache_write: 100000, cache_write_1h: 100001 },
  { input: 1000000, output: 5, reasoning: 5 },
  { input: 1000001, output: 5, reasoning: 5 },
  { input: 9007199254740991, cached: 9007199254740991, output: 9007199254740991 },
]) tierCharge(longContext, usage);
tierCharge(longContext, { input: -1 });
tierCharge(longContext, { cached: -1, output: 5 });
tierCharge({ tiers: [tier(0, { per: 1, input: "3", output: "4" })] }, { input: 1000000, output: 2 });
for (const input of [0, 1, 199999, 200000, 200001, 999999, 1000000, 1000001, 9007199254740991]) tierBound(longContext, input, 8192);
tierBound(longContext, 500000, 0);
tierBound(longContext, -1, 1);
tierBound(longContext, 1, -1);
const tierErrors = [
  { tiers: [] },
  { tiers: [tier(1, { per: 1, input: "1", output: "1" })] },
  { tiers: [tier(0, { per: 1, input: "1", output: "1" }), tier(0, { per: 1, input: "2", output: "2" })] },
  { tiers: [tier(0, { per: 1, input: "1", output: "1" }), tier(9, { per: 1, input: "2", output: "2" }), tier(5, { per: 1, input: "3", output: "3" })] },
  { tiers: [tier(0, { per: 0, input: "1", output: "1" })] },
  { tiers: [tier(0, { per: 1, input: "1", output: "1" }), tier(-5, { per: 1, input: "2", output: "2" })] },
];
for (const tiers of tierErrors) tierCharges.push({ tiers, usage: {}, expect: { error: "invalid_rate_tiers" } });

// Defaulted: the kinds a card does not give (cached, cache_write, cache_write_1h, reasoning, in that order), and of
// those the ones the usage counted.
const defaulted = [];
function defaultedCase(card, usage) {
  const missing = ["cached", "cache_write", "cache_write_1h", "reasoning"].filter((k) => card[k] === undefined);
  defaulted.push({ card, usage, expect: { all: missing, used: missing.filter((k) => (usage[k] ?? 0) > 0) } });
}
defaultedCase({ per: 1, input: "1", output: "1" }, { input: 5, cached: 4 });
defaultedCase({ per: 1, input: "1", output: "1", cached: "0" }, { cached: 4, reasoning: 2 });
defaultedCase(all, { cached: 4, reasoning: 2, cache_write: 1, cache_write_1h: 1 });
defaultedCase({ per: 1, input: "1", output: "1", cache_write: "2" }, { cache_write: 3, cache_write_1h: 3, input: 1, output: 1 });
defaultedCase({ per: 1, input: "1", output: "1" }, {});

const data = JSON.parse(readFileSync(file, "utf8"));
data.rate_card_charge = charges;
data.rate_card_bound = bounds;
data.rate_schedule_charge = scheduleCharges;
data.rate_schedule_bound = scheduleBounds;
data.rate_card_defaulted = defaulted;
data.rate_tiers_charge = tierCharges;
data.rate_tiers_bound = tierBounds;
writeFileSync(file, JSON.stringify(data, null, 1) + "\n");
console.log(`rate_card_charge: ${charges.length}, rate_card_bound: ${bounds.length}, rate_schedule_charge: ${scheduleCharges.length}, rate_schedule_bound: ${scheduleBounds.length}, rate_card_defaulted: ${defaulted.length}, rate_tiers_charge: ${tierCharges.length}, rate_tiers_bound: ${tierBounds.length}`);
