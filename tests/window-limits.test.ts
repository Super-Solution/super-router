import test from "node:test";
import assert from "node:assert/strict";
import { randomUUID, randomBytes, createHmac } from "node:crypto";
import { once } from "node:events";
import {
  PostgresStore,
  ControlPlane,
  LOCAL_DATABASE_URL,
  type PaymentIntegration,
} from "@supersolutions/super-router-storage-postgres";
import { Router, MockProvider } from "@supersolutions/super-router-core";
import {
  PlanInput,
  WindowLimits,
} from "@supersolutions/super-router-contracts";
import { z } from "@supersolutions/super-router-contracts";
import { demoService, demoInput, digestKey } from "../scripts/demo-config.js";
import { createRouterServer } from "@supersolutions/super-router-server";
const days = 86400000;
/** One chat call reserves 2144 atoms and the mock settles it at 35. */
const RESERVE = 2144n;
const CHARGE = 35n;
/** Room for exactly four reservations: 35k + 2144 <= 2250 holds for k = 0..3. */
const FOUR = "2250";
async function setup(window_limits?: unknown, credits = "100000") {
  const store = new PostgresStore(
    process.env.DATABASE_URL ?? LOCAL_DATABASE_URL,
  );
  await store.migrate();
  const workspace = "win-" + randomUUID();
  const now = new Date();
  const integration: PaymentIntegration = {
    id: "sandbox-" + randomUUID(),
    workspace,
    environment: "test",
    secret: randomBytes(32).toString("hex"),
    mode: "sandbox",
  };
  const control = new ControlPlane(store, [integration], () => now);
  const adminKey = await control.bootstrap(workspace, "test");
  const admin = await control.authenticate(adminKey.token);
  await store.publish(admin, demoService);
  await control.customer(
    admin,
    { id: "customer_demo", external_id: "external_demo" },
    "customer",
  );
  await control.member(admin, { id: "app", role: "app" }, "app");
  const issued = await control.issueKey(
    admin,
    {
      member_id: "app",
      customer_id: "customer_demo",
      scopes: ["invoke", "credits:read"],
      service_ids: ["chat"],
      expires_at: new Date(now.getTime() + 365 * days).toISOString(),
    },
    "app-key",
  );
  const app = await control.authenticate(issued.token!);
  const plan = PlanInput.parse({
    id: "windowed",
    version: 1,
    period_days: 30,
    asset: demoService.price.asset,
    credits,
    service_ids: ["chat"],
    amount_minor: "999",
    currency: "USD",
    ...(window_limits === undefined ? {} : { window_limits }),
  });
  await control.publishPlan(admin, plan, "plan");
  const sub = await control.subscribe(
    admin,
    {
      customer_id: "customer_demo",
      plan_id: plan.id,
      plan_version: plan.version,
    },
    "sub",
  );
  const order = await control.createOrder(
    admin,
    sub.id,
    integration.id,
    "order",
  );
  const raw = Buffer.from(
    JSON.stringify({
      id: randomUUID(),
      order_id: order.id,
      revision: 1,
      status: "succeeded",
      amount_minor: order.amount_minor,
      currency: order.currency,
    }),
  );
  const timestamp = Math.floor(now.getTime() / 1000).toString();
  await control.payment(
    integration.id,
    raw,
    timestamp,
    "v1=" +
      createHmac("sha256", integration.secret)
        .update(timestamp + ".")
        .update(raw)
        .digest("hex"),
  );
  const mock = new MockProvider();
  const router = new Router(store, { mock }, digestKey);
  const call = (router_ = router) =>
    router_.infer(app, demoInput, randomUUID());
  const windows = async () =>
    (await control.windows(app)) as {
      window: string;
      hours: number;
      limit: string;
      spent: string;
      held: string;
      remaining: string;
      window_start: Date | null;
      resets_at: Date | null;
    }[];
  const rows = async () =>
    (
      await store.pool.query(
        "SELECT kind,spent::text,held::text FROM sr_usage_windows WHERE workspace=$1 ORDER BY kind,window_start",
        [workspace],
      )
    ).rows as { kind: string; spent: string; held: string }[];
  /** Move every window of a kind into the past, as if that much time had gone by. */
  const age = (kind: string, hours: number) =>
    store.pool.query(
      "UPDATE sr_usage_windows SET window_start=window_start-make_interval(hours=>$3),window_end=window_end-make_interval(hours=>$3) WHERE workspace=$1 AND kind=$2",
      [workspace, kind, hours],
    );
  return {
    store,
    control,
    admin,
    app,
    adminKey,
    plan,
    sub,
    mock,
    router,
    call,
    windows,
    rows,
    age,
    workspace,
  };
}
const spentOf = async (f: Awaited<ReturnType<typeof setup>>, kind: string) =>
  (await f.windows()).find((w) => w.window === kind)!;
const refused = async (promise: Promise<unknown>) => {
  try {
    await promise;
  } catch (e: any) {
    return e as {
      code: string;
      status: number;
      retryable: boolean;
      details: any;
    };
  }
  assert.fail("expected the call to be refused");
};
test("window limits are off unless the plan enables them: no windows, no limit", async () => {
  for (const config of [
    undefined,
    { enabled: false, session: { hours: 5, credits: "1" } },
  ]) {
    const f = await setup(config);
    try {
      for (let i = 0; i < 3; i++) await f.call();
      assert.equal(f.mock.calls, 3);
      assert.deepEqual(await f.rows(), []);
      assert.deepEqual(await f.windows(), []);
      const e = await f.store.pool.query(
        "SELECT window_limits FROM sr_entitlements WHERE workspace=$1",
        [f.workspace],
      );
      assert.equal(e.rows[0].window_limits, null);
    } finally {
      await f.store.close();
    }
  }
});
test("the session window admits what fits, then refuses with its reset time and never calls the model", async () => {
  const f = await setup({
    enabled: true,
    session: { hours: 5, credits: FOUR },
  });
  try {
    for (let i = 0; i < 4; i++) await f.call();
    assert.equal(f.mock.calls, 4);
    const w = await spentOf(f, "session");
    assert.equal(w.spent, (CHARGE * 4n).toString());
    assert.equal(w.held, "0");
    assert.equal(w.limit, FOUR);
    assert.equal(w.hours, 5);
    assert.equal(
      w.resets_at!.getTime() - w.window_start!.getTime(),
      5 * 3600_000,
    );
    const e = await refused(f.call());
    assert.equal(e.code, "window_limit_reached");
    assert.equal(e.status, 429);
    assert.equal(e.retryable, true);
    assert.equal(e.details.window, "session");
    assert.equal(e.details.limit, FOUR);
    assert.equal(e.details.used, (CHARGE * 4n).toString());
    assert.equal(e.details.resets_at, w.resets_at!.toISOString());
    assert.equal(f.mock.calls, 4);
    // The refusal reserved nothing: no request row, no hold, window unchanged.
    const n = await f.store.pool.query(
      "SELECT count(*)::int AS n FROM sr_requests WHERE workspace=$1",
      [f.workspace],
    );
    assert.equal(n.rows[0].n, 4);
    assert.equal((await f.store.balance(f.app))[0].held, "0");
    assert.equal((await spentOf(f, "session")).spent, (CHARGE * 4n).toString());
  } finally {
    await f.store.close();
  }
});
test("a window reopens at the first request after it closes", async () => {
  const f = await setup({
    enabled: true,
    session: { hours: 5, credits: FOUR },
  });
  try {
    for (let i = 0; i < 4; i++) await f.call();
    await refused(f.call());
    await f.age("session", 4);
    await refused(f.call());
    await f.age("session", 2);
    await f.call();
    const w = await spentOf(f, "session");
    assert.equal(w.spent, CHARGE.toString());
    assert.equal((await f.rows()).length, 2);
    assert.equal(f.mock.calls, 5);
  } finally {
    await f.store.close();
  }
});
test("the weekly window binds independently of the session window", async () => {
  // The session fits four calls; the week fits eight (35k + 2144 <= 2400 for k = 0..7).
  const f = await setup({
    enabled: true,
    session: { hours: 5, credits: FOUR },
    weekly: { credits: "2400" },
  });
  try {
    for (let i = 0; i < 4; i++) await f.call();
    assert.equal((await refused(f.call())).details.window, "session");
    // A new session admits four more; the week still has room for exactly those.
    await f.age("session", 6);
    for (let i = 0; i < 4; i++) await f.call();
    assert.equal((await spentOf(f, "weekly")).spent, (CHARGE * 8n).toString());
    assert.equal((await spentOf(f, "session")).spent, (CHARGE * 4n).toString());
    // The session window has headroom (it is the first window's twin: 140 + 2144 <= 2250),
    // yet the week is full.
    await f.age("session", 6);
    const e = await refused(f.call());
    assert.equal(e.code, "window_limit_reached");
    assert.equal(e.details.window, "weekly");
    assert.equal(f.mock.calls, 8);
    await f.age("weekly", 169);
    await f.call();
    assert.equal((await spentOf(f, "weekly")).spent, CHARGE.toString());
  } finally {
    await f.store.close();
  }
});
test("when both windows are full the refusal names the one that resets last", async () => {
  const f = await setup({
    enabled: true,
    session: { hours: 5, credits: FOUR },
    weekly: { credits: FOUR },
  });
  try {
    for (let i = 0; i < 4; i++) await f.call();
    const e = await refused(f.call());
    assert.equal(e.details.window, "weekly");
    const w = await spentOf(f, "weekly");
    assert.equal(e.details.resets_at, w.resets_at!.toISOString());
    await f.age("weekly", 169);
    await f.age("session", 6);
    await f.call();
  } finally {
    await f.store.close();
  }
});
test("a limit below one reservation can never admit: refused as not retryable", async () => {
  const f = await setup({
    enabled: true,
    session: { hours: 5, credits: "1000" },
  });
  try {
    const e = await refused(f.call());
    assert.equal(e.code, "window_limit_reached");
    assert.equal(e.retryable, false);
    assert.equal(e.details.resets_at, null);
    assert.equal(f.mock.calls, 0);
    assert.deepEqual(await f.rows(), []);
  } finally {
    await f.store.close();
  }
});
test("a full window falls through to purchased credits, which no window limits", async () => {
  const f = await setup({
    enabled: true,
    session: { hours: 5, credits: FOUR },
  });
  try {
    await f.control.grantCredits(
      f.admin,
      {
        customer_id: "customer_demo",
        asset: f.plan.asset,
        amount: "100000",
        reason: "top-up pack",
      },
      "pack",
    );
    // 75 calls cost 2625: the window takes exactly its 2250 and the pack the other 375,
    // including the calls whose hold straddles both buckets.
    const total = 75n;
    for (let i = 0n; i < total; i++) {
      await f.call();
      assert.ok(BigInt((await spentOf(f, "session")).spent) <= 2250n);
    }
    assert.equal((await spentOf(f, "session")).spent, "2250");
    assert.equal((await spentOf(f, "session")).held, "0");
    const buckets = (await f.control.buckets(f.app)) as any[];
    const sub = buckets.find((b) => b.source_kind === "subscription");
    const pack = buckets.find((b) => b.source_kind !== "subscription");
    assert.equal(sub.remaining, (100000n - 2250n).toString());
    assert.equal(
      pack.remaining,
      (100000n - (total * CHARGE - 2250n)).toString(),
    );
    assert.equal(sub.held, "0");
    assert.equal(pack.held, "0");
  } finally {
    await f.store.close();
  }
});
test("a refund gives its window headroom back", async () => {
  const f = await setup({
    enabled: true,
    session: { hours: 5, credits: FOUR },
  });
  try {
    const first: any = await f.call();
    for (let i = 0; i < 3; i++) await f.call();
    await refused(f.call());
    await f.store.refund(
      { ...f.admin, customer: "customer_demo" },
      first.id,
      "customer_sale",
      CHARGE.toString(),
      "refund-1",
      "goodwill credit",
    );
    assert.equal((await spentOf(f, "session")).spent, (CHARGE * 3n).toString());
    await f.call();
    assert.equal((await spentOf(f, "session")).spent, (CHARGE * 4n).toString());
  } finally {
    await f.store.close();
  }
});
test("a failed or released request holds nothing and does not start the window clock", async () => {
  const f = await setup({
    enabled: true,
    session: { hours: 5, credits: FOUR },
    weekly: { credits: "100000" },
  });
  try {
    const failing = new Router(
      f.store,
      { mock: new MockProvider("reject") },
      digestKey,
    );
    const failed: any = await f.call(failing);
    assert.equal(failed.billing_status, "released");
    assert.deepEqual(await f.rows(), []);
    assert.equal((await f.store.balance(f.app))[0].held, "0");
    await f.call();
    const w = await spentOf(f, "session");
    assert.equal(w.spent, CHARGE.toString());
    assert.equal(w.held, "0");
  } finally {
    await f.store.close();
  }
});
test("concurrent calls cannot overshoot a window", async () => {
  const f = await setup({
    enabled: true,
    session: { hours: 5, credits: FOUR },
  });
  try {
    const results = await Promise.allSettled(
      Array.from({ length: 12 }, () => f.call()),
    );
    const ok = results.filter((r) => r.status === "fulfilled").length;
    assert.ok(ok >= 1 && ok <= 4, "admitted " + ok);
    for (const r of results)
      if (r.status === "rejected")
        assert.equal((r.reason as any).code, "window_limit_reached");
    const w = await spentOf(f, "session");
    assert.equal(w.spent, (CHARGE * BigInt(ok)).toString());
    assert.equal(w.held, "0");
    assert.equal(f.mock.calls, ok);
  } finally {
    await f.store.close();
  }
});
test("the paid cycle keeps the limits of the plan version that granted it", async () => {
  const f = await setup({
    enabled: true,
    session: { hours: 5, credits: FOUR },
    weekly: { credits: "20000" },
  });
  try {
    const e = await f.store.pool.query(
      "SELECT window_limits FROM sr_entitlements WHERE workspace=$1",
      [f.workspace],
    );
    assert.deepEqual(e.rows[0].window_limits, {
      enabled: true,
      session: { hours: 5, credits: FOUR },
      weekly: { credits: "20000" },
    });
    const view = await f.windows();
    assert.deepEqual(
      view.map((w) => [w.window, w.hours, w.limit, w.spent, w.remaining]),
      [
        ["session", 5, FOUR, "0", FOUR],
        ["weekly", 168, "20000", "0", "20000"],
      ],
    );
    assert.equal(view[0]!.window_start, null);
    assert.equal(view[0]!.resets_at, null);
    // An unrelated customer's key cannot read it.
    await assert.rejects(
      () => f.control.windows(f.app, "someone_else"),
      /scope_denied/,
    );
  } finally {
    await f.store.close();
  }
});
test("plan window limits are validated and old plans stay valid", () => {
  const base = {
    id: "p",
    version: 1,
    period_days: 30,
    asset: "credit:demo",
    credits: "1000",
    service_ids: ["chat"],
    amount_minor: "999",
    currency: "USD",
  };
  assert.ok(PlanInput.safeParse(base).success);
  const ok = (w: unknown) => WindowLimits.safeParse(w).success;
  assert.ok(ok({ enabled: true, session: { hours: 5, credits: "10" } }));
  assert.ok(ok({ enabled: true, weekly: { credits: "10" } }));
  assert.ok(
    ok({
      enabled: true,
      session: { hours: 72, credits: "10" },
      weekly: { credits: "10" },
    }),
  );
  assert.ok(ok({ enabled: false }));
  assert.equal(ok({ enabled: true }), false, "nothing to enforce");
  assert.equal(
    ok({
      enabled: true,
      session: { hours: 5, credits: "11" },
      weekly: { credits: "10" },
    }),
    false,
    "session above weekly",
  );
  assert.equal(
    ok({ enabled: true, session: { hours: 0, credits: "1" } }),
    false,
  );
  assert.equal(
    ok({ enabled: true, session: { hours: 73, credits: "1" } }),
    false,
  );
  assert.equal(
    ok({ enabled: true, session: { hours: 1.5, credits: "1" } }),
    false,
  );
  assert.equal(
    ok({ enabled: true, session: { hours: 5, credits: "0" } }),
    false,
  );
  assert.equal(ok({ enabled: true, weekly: { credits: "1", days: 7 } }), false);
  assert.equal(ok({ enabled: true, monthly: { credits: "1" } }), false);
  assert.equal(
    PlanInput.safeParse({
      ...base,
      window_limits: { enabled: true, weekly: { credits: "5" } },
    }).success,
    true,
  );
  assert.doesNotThrow(() =>
    z.toJSONSchema(PlanInput, { target: "draft-2020-12" }),
  );
});
test("HTTP: the refusal carries its window details and customers can read their usage", async () => {
  const f = await setup({
    enabled: true,
    session: { hours: 5, credits: FOUR },
  });
  const server = createRouterServer(f.router, f.store, f.control);
  try {
    server.listen(0, "127.0.0.1");
    await once(server, "listening");
    const url = "http://127.0.0.1:" + (server.address() as any).port;
    const send = (path: string, method = "GET", body?: unknown) =>
      fetch(url + path, {
        method,
        headers: {
          Authorization: "Bearer " + f.adminKey.token,
          "Content-Type": "application/json",
          "Idempotency-Key": randomUUID(),
        },
        body: body === undefined ? undefined : JSON.stringify(body),
      });
    const app = await f.control.issueKey(
      f.admin,
      {
        member_id: "app",
        customer_id: "customer_demo",
        scopes: ["invoke", "credits:read"],
        service_ids: ["chat"],
        expires_at: new Date(Date.now() + days).toISOString(),
      },
      "http-app-key",
    );
    const infer = () =>
      fetch(url + "/v1/inferences", {
        method: "POST",
        headers: {
          Authorization: "Bearer " + app.token,
          "Content-Type": "application/json",
          "Idempotency-Key": randomUUID(),
        },
        body: JSON.stringify(demoInput),
      });
    for (let i = 0; i < 4; i++) assert.equal((await infer()).status, 200);
    const blocked = await infer();
    assert.equal(blocked.status, 429);
    const body: any = await blocked.json();
    assert.equal(body.error.code, "window_limit_reached");
    assert.equal(body.error.retryable, true);
    assert.equal(body.error.details.window, "session");
    const own: any = await (
      await fetch(url + "/v1/windows", {
        headers: { Authorization: "Bearer " + app.token },
      })
    ).json();
    assert.equal(own.windows[0].spent, (CHARGE * 4n).toString());
    assert.equal(
      own.windows[0].remaining,
      (BigInt(FOUR) - CHARGE * 4n).toString(),
    );
    const admin: any = await (
      await send("/v1/admin/customers/customer_demo/windows")
    ).json();
    assert.equal(admin.windows[0].window, "session");
    const billing: any = await f.control.customerBilling(f.admin, f.app);
    assert.equal(billing.windows[0].limit, FOUR);
  } finally {
    server.close();
    await f.store.close();
  }
});
void RESERVE;
