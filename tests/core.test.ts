import test from "node:test";
import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { Router, MockProvider, charge, bound, route } from "@superrouter/core";
import { Service, type Principal } from "@superrouter/contracts";
import {
  PostgresStore,
  LOCAL_DATABASE_URL,
} from "@superrouter/storage-postgres";
import {
  demoService,
  demoInput,
  principal,
  digestKey,
  seed,
} from "../scripts/demo-config.js";
async function fixture(config = demoService, amount = "100000") {
  const store = new PostgresStore(
    process.env.DATABASE_URL ?? LOCAL_DATABASE_URL,
  );
  await store.migrate();
  const p = { ...principal, workspace: "test-" + randomUUID() };
  await seed(store, p, config, amount);
  return { store, p };
}
test("exact rational rounding, large values, cache categories, conservative bound", () => {
  assert.equal(
    charge(
      { input: 1, cached: 1, cache_write: 1, output: 1 },
      {
        asset: "USD",
        input: "1",
        cached: "1",
        cache_write: "1",
        output: "1",
        per: 3,
      },
    ),
    2n,
  );
  assert.equal(
    charge(
      { input: 2, cached: 0, cache_write: 0, output: 0 },
      {
        asset: "USD",
        input: "9007199254740993",
        cached: "0",
        cache_write: "0",
        output: "0",
        per: 1,
      },
    ),
    18014398509481986n,
  );
  assert.equal(
    bound(10, 5, {
      asset: "USD",
      input: "1",
      cached: "2",
      cache_write: "3",
      output: "4",
      per: 1,
    }),
    50n,
  );
});
test("routing applies hard region/model filters before cheap scores", () => {
  const s = Service.parse({
    ...demoService,
    mode: "specific",
    model: "mock-model",
    offerings: [
      { ...demoService.offerings[0], id: "wrong-region", region: "elsewhere" },
      { ...demoService.offerings[0], id: "wrong-model", model: "other" },
      demoService.offerings[0],
    ],
  });
  const d = route(s, demoInput);
  assert.equal(d.candidates.length, 1);
  assert.deepEqual(
    d.excluded.map((x) => x.reason),
    ["region_mismatch", "model_mismatch"],
  );
});
test("real DB settles once, replays after price changes, and keeps history immutable", async () => {
  const { store, p } = await fixture();
  try {
    const mock = new MockProvider();
    const router = new Router(store, { mock }, digestKey);
    const r = await router.infer(p, demoInput, "same");
    assert.equal(r.billing_status, "settled");
    assert.equal(mock.calls, 1);
    await store.publish(p, {
      ...demoService,
      version: 2,
      price: { ...demoService.price, input: "900000" },
    });
    const repeated = await router.infer(p, demoInput, "same");
    assert.equal(repeated.id, r.id);
    assert.equal(mock.calls, 1);
    await assert.rejects(
      () =>
        router.infer(
          p,
          {
            ...demoInput,
            input: { messages: [{ role: "user", content: "different" }] },
          },
          "same",
        ),
      /idempotency_conflict/,
    );
    const balances = await store.balance(p);
    assert.equal(balances[0].balance, "99965");
    assert.equal(balances[0].held, "0");
    const t = await store.trace(p, r.id);
    assert.equal(t.request.service.version, 1);
    assert.equal(t.attempts[0].cost, "15");
    assert.equal(t.settlements.length, 1);
    await assert.rejects(
      () =>
        store.pool.query("UPDATE sr_journal SET reason=$1 WHERE workspace=$2", [
          "tamper",
          p.workspace,
        ]),
      /append-only/,
    );
  } finally {
    await store.close();
  }
});
test("concurrent request reservations cannot overspend; rejected work never dispatches", async () => {
  const { store, p } = await fixture(demoService, "2144");
  try {
    const mock = new MockProvider("success", 100);
    const router = new Router(store, { mock }, digestKey);
    const results = await Promise.allSettled(
      Array.from({ length: 8 }, (_, i) =>
        router.infer(p, demoInput, "concurrent-" + i),
      ),
    );
    assert.equal(results.filter((x) => x.status === "fulfilled").length, 1);
    assert.equal(mock.calls, 1);
    assert.equal((await store.balance(p))[0].held, "0");
  } finally {
    await store.close();
  }
});
test("concurrent identical idempotency key executes only once", async () => {
  const { store, p } = await fixture();
  try {
    const mock = new MockProvider("success", 100);
    const router = new Router(store, { mock }, digestKey);
    const results = await Promise.all(
      Array.from({ length: 5 }, () => router.infer(p, demoInput, "dupe")),
    );
    assert.equal(new Set(results.map((x) => x.id)).size, 1);
    assert.equal(mock.calls, 1);
  } finally {
    await store.close();
  }
});
test("fallback records each attempt, costs, and bills only successful usage", async () => {
  const s = Service.parse({
    ...demoService,
    offerings: [
      { ...demoService.offerings[0], id: "a", provider: "reject" },
      { ...demoService.offerings[0], id: "b", provider: "mock" },
    ],
  });
  const { store, p } = await fixture(s);
  try {
    const router = new Router(
      store,
      { mock: new MockProvider(), reject: new MockProvider("reject") },
      digestKey,
    );
    const r = await router.infer(p, demoInput, "fallback");
    const t = await store.trace(p, r.id);
    assert.equal(r.execution_status, "succeeded");
    assert.deepEqual(
      t.attempts.map((a) => a.status),
      ["failed", "succeeded"],
    );
    assert.deepEqual(
      t.attempts.map((a) => a.cost),
      ["0", "15"],
    );
    assert.equal(t.settlements[0].amount, "35");
  } finally {
    await store.close();
  }
});
test("partial/unknown output retains holds, forbids fallback, survives reopening DB", async () => {
  const s = Service.parse({
    ...demoService,
    offerings: [
      { ...demoService.offerings[0], id: "a", provider: "partial" },
      { ...demoService.offerings[0], id: "b", provider: "mock" },
    ],
  });
  const { store, p } = await fixture(s);
  let id = "";
  try {
    const fallback = new MockProvider();
    const router = new Router(
      store,
      { partial: new MockProvider("partial"), mock: fallback },
      digestKey,
    );
    const r = await router.infer(p, demoInput, "partial");
    id = r.id;
    assert.equal(r.execution_status, "partial");
    assert.equal(r.billing_status, "pending");
    assert.equal(fallback.calls, 0);
    assert.equal((await store.balance(p))[0].held, "2144");
  } finally {
    await store.close();
  }
  const reopened = new PostgresStore(
    process.env.DATABASE_URL ?? LOCAL_DATABASE_URL,
  );
  try {
    assert.equal((await reopened.get(p, id)).billing, "pending");
    await assert.rejects(
      () =>
        reopened.resolveWaiver(
          { ...p, scopes: ["invoke"] },
          id,
          "operator confirmed waiver",
        ),
      /scope_denied/,
    );
    await reopened.resolveWaiver(p, id, "operator verified no customer charge");
    assert.equal((await reopened.balance(p))[0].held, "0");
  } finally {
    await reopened.close();
  }
});
test("all-or-nothing platform/customer holds and tenant isolation", async () => {
  const s = Service.parse({
    ...demoService,
    platform_price: {
      ...demoService.price,
      asset: "credit:platform",
      input: "1000000",
    },
  });
  const { store, p } = await fixture(s);
  try {
    const mock = new MockProvider();
    const router = new Router(store, { mock }, digestKey);
    await assert.rejects(
      () => router.infer(p, demoInput, "dual"),
      /insufficient_balance/,
    );
    assert.equal(mock.calls, 0);
    assert.ok((await store.balance(p)).every((a) => a.held === "0"));
    await assert.rejects(
      () => store.getService({ ...p, workspace: "other" }, s.id),
      /resource_not_found/,
    );
    await assert.rejects(
      () => store.publish(p, { ...demoService, version: 2 }, "cloud"),
      /byok_not_allowed/,
    );
    await assert.rejects(
      () =>
        store.grant(
          { ...p, scopes: ["invoke"] },
          "customer_sale",
          "credit:demo",
          "10",
          "bad",
        ),
      /scope_denied/,
    );
  } finally {
    await store.close();
  }
});
test("expired bucket is removed through journal and cannot be reserved", async () => {
  const { store, p } = await fixture(demoService, "1");
  try {
    await store.grant(
      p,
      "customer_sale",
      demoService.price.asset,
      "5000",
      "expired",
      new Date(Date.now() + 86400000),
    );
    await store.pool.query(
      "UPDATE sr_buckets SET expires_at=clock_timestamp()-interval '1 second' WHERE workspace=$1 AND environment=$2 AND source='expired'",
      [p.workspace, p.environment],
    );
    assert.equal((await store.balance(p))[0].available, "1");
    const router = new Router(store, { mock: new MockProvider() }, digestKey);
    await assert.rejects(
      () => router.infer(p, demoInput, "expiry"),
      /insufficient_balance/,
    );
  } finally {
    await store.close();
  }
});
test("crash after dispatch stays unknown; outbox fencing rejects stale acknowledgement", async () => {
  const { store, p } = await fixture();
  try {
    const legs = await store.accounts(p, demoService);
    legs[0]!.reserved = "2144";
    const r = await store.reserve(
      p,
      "crash",
      "hash",
      demoService,
      legs,
      new Date(Date.now() + 60000).toISOString(),
    );
    await store.begin(p, r.request.id, demoService.offerings[0]!, "1056");
    await store.pool.query(
      "UPDATE sr_requests SET deadline=clock_timestamp()-interval '1 second' WHERE workspace=$1 AND environment=$2 AND id=$3",
      [p.workspace, p.environment, r.request.id],
    );
    await store.recover();
    assert.equal((await store.get(p, r.request.id)).billing, "pending");
    assert.equal((await store.balance(p))[0].held, "2144");
    const events = await store.claimEvents(100, p);
    const e = events.find(
      (x) => (x.payload as any).request_id === r.request.id,
    )!;
    assert.ok(e);
    assert.equal(await store.ackEvent(e.id, e.generation - 1), false);
    assert.equal(await store.ackEvent(e.id, e.generation), true);
    assert.equal(await store.ackEvent(e.id, e.generation), false);
  } finally {
    await store.close();
  }
});
test("refund is capped, idempotent, and returns to original expired bucket without extending it", async () => {
  const { store, p } = await fixture();
  try {
    const router = new Router(store, { mock: new MockProvider() }, digestKey);
    const r = await router.infer(p, demoInput, "refundable");
    await store.refund(
      p,
      r.id,
      "customer_sale",
      "10",
      "refund-1",
      "customer requested refund",
    );
    await store.refund(
      p,
      r.id,
      "customer_sale",
      "10",
      "refund-1",
      "customer requested refund",
    );
    assert.equal((await store.balance(p))[0].balance, "99975");
    await assert.rejects(
      () =>
        store.refund(p, r.id, "customer_sale", "26", "refund-2", "over refund"),
      /refund_exceeded/,
    );
    await store.pool.query(
      "UPDATE sr_buckets SET expires_at=now()-interval '1 second' WHERE workspace=$1",
      [p.workspace],
    );
    await store.balance(p);
    await store.refund(
      p,
      r.id,
      "customer_sale",
      "25",
      "refund-3",
      "refund after expiry",
    );
    assert.equal((await store.balance(p))[0].balance, "0");
  } finally {
    await store.close();
  }
});
test("grant retries compare original intent, immutable published versions cannot be altered in SQL", async () => {
  const { store, p } = await fixture();
  try {
    await assert.rejects(
      () =>
        store.grant(
          p,
          "customer_sale",
          demoService.price.asset,
          "200000",
          "initial-demo-grant",
        ),
      /idempotency_conflict/,
    );
    await assert.rejects(
      () =>
        store.pool.query(
          "UPDATE sr_services SET config=jsonb_set(config,'{max_attempts}','5') WHERE workspace=$1",
          [p.workspace],
        ),
      /immutable/,
    );
  } finally {
    await store.close();
  }
});
test("worker completes recorded successful usage after crash before settlement", async () => {
  const { store, p } = await fixture();
  try {
    const legs = await store.accounts(p, demoService);
    legs[0]!.reserved = "2144";
    const r = await store.reserve(
      p,
      "crash-success",
      "hash",
      demoService,
      legs,
      new Date(Date.now() + 60000).toISOString(),
    );
    const a = await store.begin(
      p,
      r.request.id,
      demoService.offerings[0]!,
      "1056",
    );
    await store.attemptResult(
      p,
      r.request.id,
      a,
      "succeeded",
      { input: 10, cached: 0, cache_write: 0, output: 5 },
      "15",
    );
    await store.pool.query(
      "UPDATE sr_requests SET deadline=clock_timestamp()-interval '1 second' WHERE workspace=$1 AND environment=$2 AND id=$3",
      [p.workspace, p.environment, r.request.id],
    );
    await store.recover();
    assert.equal((await store.get(p, r.request.id)).billing, "settled");
    assert.equal((await store.balance(p))[0].balance, "99965");
  } finally {
    await store.close();
  }
});
test("known partial terminal settles once; unknown stream holds are a different case", async () => {
  const { store, p } = await fixture();
  try {
    const partial = {
      async *invoke() {
        yield { type: "delta" as const, text: "partial" };
        yield {
          type: "usage" as const,
          usage: { input: 10, cached: 0, cache_write: 0, output: 5 },
        };
        yield { type: "done" as const, completion: "partial" as const };
      },
    };
    const router = new Router(store, { mock: partial }, digestKey);
    const r = await router.infer(p, demoInput, "partial-known");
    assert.equal(r.execution_status, "partial");
    assert.equal(r.billing_status, "settled");
    assert.equal((await store.balance(p))[0].balance, "99965");
  } finally {
    await store.close();
  }
});
test("expired pre-dispatch reservation is safely released and has no attempts", async () => {
  const { store, p } = await fixture();
  try {
    const legs = await store.accounts(p, demoService);
    legs[0]!.reserved = "2144";
    const r = await store.reserve(
      p,
      "before-dispatch",
      "hash",
      demoService,
      legs,
      new Date(Date.now() + 60000).toISOString(),
    );
    await store.pool.query(
      "UPDATE sr_requests SET deadline=clock_timestamp()-interval '1 second' WHERE workspace=$1 AND environment=$2 AND id=$3",
      [p.workspace, p.environment, r.request.id],
    );
    await store.recover();
    assert.equal((await store.get(p, r.request.id)).billing, "released");
    assert.equal((await store.balance(p))[0].held, "0");
    assert.equal((await store.trace(p, r.request.id)).attempts.length, 0);
  } finally {
    await store.close();
  }
});
test("supply upper-bound violation becomes reconciliation, never hidden confirmed settlement", async () => {
  const { store, p } = await fixture();
  try {
    const bad = {
      async *invoke() {
        yield {
          type: "usage" as const,
          usage: { input: 10000, cached: 0, cache_write: 0, output: 5 },
        };
        yield { type: "done" as const };
      },
    };
    const router = new Router(store, { mock: bad }, digestKey);
    const r = await router.infer(p, demoInput, "over-bound");
    assert.equal(r.billing_status, "pending");
    assert.equal((await store.balance(p))[0].held, "2144");
  } finally {
    await store.close();
  }
});
