// conformance/dataplane.v1.json is hand-authored and normative. It must hold for every DataStore:
// the in-memory reference and PostgreSQL here, and the Go and Python ports against their own stores.
import { readFileSync } from "node:fs";
import test from "node:test";
import { MemoryStore } from "@supersolutions/super-router-storage-memory";
import {
  PostgresStore,
  LOCAL_DATABASE_URL,
} from "@supersolutions/super-router-storage-postgres";
import { runScenario, type Harness } from "./support/dataplane-runner.js";

const data = JSON.parse(
  readFileSync(
    new URL("../conformance/dataplane.v1.json", import.meta.url),
    "utf8",
  ),
);

const memory = async (): Promise<Harness> => {
  let offset = 0;
  const clock = () => Date.now() + offset;
  const store = new MemoryStore(clock);
  return {
    store,
    clock,
    advance: async (ms) => void (offset += ms),
    buckets: async (p) =>
      store.inspect(p).buckets.sort((a, b) => (a.source < b.source ? -1 : 1)),
  };
};

const pg = new PostgresStore(process.env.DATABASE_URL ?? LOCAL_DATABASE_URL);
const postgres = async (): Promise<Harness> => {
  // recover() is global: settle what earlier runs left expired so a scenario counts only its own.
  await pg.recover();
  return {
    store: pg,
    clock: Date.now,
    advance: (ms) => new Promise((r) => setTimeout(r, ms)),
    buckets: async (p) =>
      (
        await pg.pool.query(
          "SELECT source,remaining::text,held::text FROM sr_buckets WHERE workspace=$1 AND environment=$2 ORDER BY source",
          [p.workspace, p.environment],
        )
      ).rows,
  };
};

test.before(async () => {
  await pg.migrate();
});
test.after(async () => {
  await pg.close();
});
for (const [name, make] of [
  ["memory", memory],
  ["postgres", postgres],
] as const)
  for (const scenario of data.scenarios)
    test(`dataplane/${name}: ${scenario.name}`, () =>
      runScenario(data, scenario, make));
