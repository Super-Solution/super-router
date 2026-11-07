// The TypeScript reference must still produce every expectation in conformance/pricing.v1.json,
// the data the Go and Python ports are tested against. Regenerate with
// `npx tsx scripts/export-conformance.ts` when pricing behaviour changes on purpose.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import {
  bound,
  charge,
  compileCreditPrice,
} from "@supersolutions/super-router-core";
import { RouterError } from "@supersolutions/super-router-contracts";

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
