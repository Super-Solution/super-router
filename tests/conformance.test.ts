// The TypeScript reference must still produce every expectation in conformance/*.v1.json,
// the data the Go and Python ports are tested against. Regenerate with
// `npx tsx scripts/export-conformance.ts` (and `scripts/export-routing-conformance.ts` for
// routing) when behaviour changes on purpose.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import {
  bound,
  canonical,
  charge,
  compileCreditPrice,
  route,
} from "@supersolutions/super-router-core";
import {
  Invocation,
  RouterError,
  Service,
} from "@supersolutions/super-router-contracts";

const cases = JSON.parse(
  readFileSync(
    new URL("../conformance/pricing.v1.json", import.meta.url),
    "utf8",
  ),
);
function run(fn: () => unknown) {
  try {
    const value = fn();
    return { ok: typeof value === "bigint" ? value.toString() : value };
  } catch (e) {
    if (e instanceof RouterError) return { error: e.code };
    throw e;
  }
}

test("charge matches the conformance data", () => {
  for (const c of cases.charge)
    assert.deepEqual(
      run(() => charge(c.usage, c.price)),
      c.expect,
    );
});
test("bound matches the conformance data", () => {
  for (const c of cases.bound)
    assert.deepEqual(
      run(() => bound(c.input, c.output, c.price)),
      c.expect,
    );
});
test("compileCreditPrice matches the conformance data", () => {
  for (const c of cases.compile_credit_price)
    assert.deepEqual(
      run(() => compileCreditPrice(c.config)),
      c.expect,
    );
});

const routing = JSON.parse(
  readFileSync(
    new URL("../conformance/routing.v1.json", import.meta.url),
    "utf8",
  ),
);
test("route matches the conformance data", () => {
  assert.ok(routing.route.length >= 300);
  for (const c of routing.route)
    assert.deepEqual(
      run(() => {
        const r = route(c.service, c.invocation);
        return {
          candidates: r.candidates.map((x) => ({
            id: x.offering.id,
            cost: x.cost.toString(),
            score: x.score,
          })),
          excluded: r.excluded,
        };
      }),
      c.expect,
    );
});

const parsing = JSON.parse(
  readFileSync(
    new URL("../conformance/contracts.v1.json", import.meta.url),
    "utf8",
  ),
);
test("Service and Invocation parsing match the conformance data", () => {
  assert.ok(parsing.service.length >= 50 && parsing.invocation.length >= 30);
  for (const [schema, list, withCanonical] of [
    [Service, parsing.service, false],
    [Invocation, parsing.invocation, true],
  ] as const)
    for (const c of list) {
      const r = schema.safeParse(c.input);
      if (c.expect.error) assert.equal(r.success, false, c.name);
      else {
        assert.ok(r.success, c.name);
        assert.deepEqual(
          JSON.parse(JSON.stringify(r.data)),
          c.expect.ok,
          c.name,
        );
        if (withCanonical)
          assert.equal(canonical(r.data), c.expect.canonical, c.name);
      }
    }
});
