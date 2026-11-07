import test from "node:test";
import assert from "node:assert/strict";
import {
  PostgresStore,
  ControlPlane,
  LOCAL_DATABASE_URL,
} from "@supersolutions/super-router-storage-postgres";
import { runRecoveryWorker } from "@supersolutions/super-router-worker";

test("recovery worker loops until its signal aborts", async () => {
  const store = new PostgresStore(process.env.DATABASE_URL ?? LOCAL_DATABASE_URL);
  try {
    await store.migrate();
    const control = new ControlPlane(store);
    const abort = new AbortController();
    let passes = 0;
    const original = store.recover.bind(store);
    store.recover = async () => {
      passes += 1;
      if (passes === 2) abort.abort();
      return original();
    };
    await runRecoveryWorker(store, control, {
      signal: abort.signal,
      intervalMs: 5,
    });
    assert.equal(passes, 2);
  } finally {
    await store.close();
  }
});
