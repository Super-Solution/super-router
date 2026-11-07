#!/usr/bin/env node
import {
  PostgresStore,
  ControlPlane,
  LOCAL_DATABASE_URL,
} from "@supersolutions/super-router-storage-postgres";
import { runRecoveryWorker } from "./index.js";

const store = new PostgresStore(process.env.DATABASE_URL ?? LOCAL_DATABASE_URL);
await store.migrate();
const control = new ControlPlane(store);
const abort = new AbortController();
for (const s of ["SIGINT", "SIGTERM"] as const)
  process.once(s, () => abort.abort());
console.log(
  "Recovery worker started. Outbox retained for a configured subscriber; no fake delivery acknowledgements.",
);
try {
  await runRecoveryWorker(store, control, {
    signal: abort.signal,
    onRecovered: (n) => console.log("Recovered requests:", n),
  });
} finally {
  await store.close();
}
