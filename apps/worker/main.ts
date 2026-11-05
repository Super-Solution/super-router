import {
  PostgresStore,
  ControlPlane,
  LOCAL_DATABASE_URL,
} from "@superrouter/storage-postgres";
const store = new PostgresStore(process.env.DATABASE_URL ?? LOCAL_DATABASE_URL);
await store.migrate();
const control = new ControlPlane(store);
let stop = false;
for (const s of ["SIGINT", "SIGTERM"] as const)
  process.once(s, () => {
    stop = true;
  });
console.log(
  "Recovery worker started. Outbox retained for a configured subscriber; no fake delivery acknowledgements.",
);
try {
  while (!stop) {
    const n = await store.recover();
    await control.advancePeriods();
    if (n) console.log("Recovered requests:", n);
    await new Promise((r) => setTimeout(r, 1000));
  }
} finally {
  await store.close();
}
