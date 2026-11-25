// Writes conformance/retry_after.v1.json: how the value of a Retry-After header is read.
//
// The expected answers come from the small model below, written independently of the Rust and Go readers: only
// the delta-seconds form (a non-negative number, whole or with a fraction) is read, in milliseconds rounded down and
// capped at one day. The HTTP-date form is not read.
//
//   node scripts/export-retry-after-conformance.mjs
import { writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const file = join(dirname(fileURLToPath(import.meta.url)), "..", "conformance", "retry_after.v1.json");
const CAP = 86_400_000;

function read(value) {
  const v = value.trim();
  if (!/^\d+(\.\d+)?$/.test(v)) return null;
  const [whole, fraction = ""] = v.split(".");
  const ms = BigInt(whole) * 1000n + BigInt((fraction + "000").slice(0, 3));
  return ms > BigInt(CAP) ? CAP : Number(ms);
}

const inputs = [
  "30", "0", "1", "120", "1.5", "0.25", "2.0005", "0.001", "0.0009", " 7 ", "\t12\t", "007",
  "86400", "86401", "999999999", "99999999999999999999999", "86400.999",
  "", " ", "-1", "+5", "1e3", "abc", "5s", "5 seconds", "1.", ".5", "1.2.3", "1,5",
  "Wed, 21 Oct 2026 07:28:00 GMT", "2026-10-21T07:28:00Z", "0x10", "٣",
];

const cases = inputs.map((header) => ({ header, expect_ms: read(header) }));
writeFileSync(
  file,
  JSON.stringify({ version: 1, description: "Retry-After header (delta-seconds form) to milliseconds, or null. Written by scripts/export-retry-after-conformance.mjs; Shared by every backend that implements it (see conformance/README.md).", cases }, null, 1) + "\n",
);
console.log(`retry_after: ${cases.length} cases`);
