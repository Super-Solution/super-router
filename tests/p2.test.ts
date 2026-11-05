import test from "node:test";
import assert from "node:assert/strict";
import { randomUUID, randomBytes, createHmac } from "node:crypto";
import { once } from "node:events";
import {
  PostgresStore,
  ControlPlane,
  LOCAL_DATABASE_URL,
  type PaymentIntegration,
} from "@superrouter/storage-postgres";
import { Router, MockProvider } from "@superrouter/core";
import { PlanInput, type Principal } from "@superrouter/contracts";
import { demoService, demoInput, digestKey } from "../scripts/demo-config.js";
import { createRouterServer } from "../apps/server/server.js";
const days = 86400000;
async function setup() {
  const store = new PostgresStore(
    process.env.DATABASE_URL ?? LOCAL_DATABASE_URL,
  );
  await store.migrate();
  const workspace = "p2-" + randomUUID();
  let now = new Date();
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
    id: "basic",
    version: 1,
    period_days: 30,
    asset: demoService.price.asset,
    credits: "2144",
    service_ids: ["chat"],
    amount_minor: "999",
    currency: "USD",
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
  const pay = async (
    order: any,
    status = "succeeded",
    id = randomUUID(),
    revision = 1,
    overrides = {},
  ) => {
    const payload = {
      id,
      order_id: order.id,
      revision,
      status,
      amount_minor: order.amount_minor,
      currency: order.currency,
      ...overrides,
    };
    const raw = Buffer.from(JSON.stringify(payload));
    const timestamp = Math.floor(now.getTime() / 1000).toString();
    const signature =
      "v1=" +
      createHmac("sha256", integration.secret)
        .update(timestamp + ".")
        .update(raw)
        .digest("hex");
    return control.payment(integration.id, raw, timestamp, signature);
  };
  return {
    store,
    control,
    admin,
    app,
    issued,
    adminKey,
    plan,
    sub,
    integration,
    pay,
    advance: (n: number) => {
      now = new Date(now.getTime() + n * days);
    },
    now: () => now,
  };
}
test("P2 roles and keys persist; cannot escalate, remove last owner, or reuse rotated/revoked credentials", async () => {
  const f = await setup();
  try {
    await assert.rejects(
      () =>
        f.control.issueKey(
          f.admin,
          {
            member_id: "app",
            customer_id: "customer_demo",
            scopes: ["billing:write"],
            service_ids: ["chat"],
            expires_at: new Date(Date.now() + days).toISOString(),
          },
          "escalate",
        ),
      /scope_escalation/,
    );
    await assert.rejects(
      () =>
        f.control.member(
          f.admin,
          { id: "owner", role: "viewer" },
          "last-owner",
        ),
      /last_owner/,
    );
    const fresh = new ControlPlane(f.store);
    assert.equal(
      (await fresh.authenticate(f.issued.token!)).customer,
      "customer_demo",
    );
    const rotated = await f.control.rotateKey(f.admin, f.issued.id, "rotate");
    await assert.rejects(
      () => fresh.authenticate(f.issued.token!),
      /invalid_key/,
    );
    assert.ok(
      (await fresh.authenticate(rotated.token!)).scopes.includes("invoke"),
    );
    const replay = await f.control.rotateKey(f.admin, f.issued.id, "rotate");
    assert.equal(replay.token, null);
    await f.control.revokeKey(f.admin, rotated.id, "revoke");
    await assert.rejects(
      () => fresh.authenticate(rotated.token!),
      /invalid_key/,
    );
  } finally {
    await f.store.close();
  }
});
test("P2 role downgrade applies to already-issued keys and stale principal cannot reserve", async () => {
  const f = await setup();
  try {
    await f.control.grantCredits(
      f.admin,
      {
        customer_id: "customer_demo",
        asset: f.plan.asset,
        amount: "10000",
        reason: "test promotion",
      },
      "grant",
    );
    await f.control.member(f.admin, { id: "app", role: "viewer" }, "downgrade");
    const after = await f.control.authenticate(f.issued.token!);
    assert.equal(after.scopes.includes("invoke"), false);
    const mock = new MockProvider(),
      router = new Router(f.store, { mock }, digestKey);
    await assert.rejects(
      () => router.infer(f.app, demoInput, "stale-principal"),
      /scope_denied/,
    );
    assert.equal(mock.calls, 0);
    assert.equal((await f.store.balance(f.app))[0].held, "0");
  } finally {
    await f.store.close();
  }
});
test("P2 signed payment event and business order dedupe grants once under concurrency", async () => {
  const f = await setup();
  try {
    const o = await f.control.createOrder(
      f.admin,
      f.sub.id,
      f.integration.id,
      "order",
    );
    const e = randomUUID();
    await Promise.all([
      f.pay(o, "succeeded", e),
      f.pay(o, "succeeded", e),
      f.pay(o, "succeeded", randomUUID(), 2),
    ]);
    assert.equal((await f.store.balance(f.app))[0].balance, "2144");
    const rows = await f.store.pool.query(
      "SELECT * FROM sr_entitlements WHERE workspace=$1",
      [f.admin.workspace],
    );
    assert.equal(rows.rowCount, 1);
    assert.equal(
      (await f.control.getSubscription(f.admin, f.sub.id)).status,
      "active",
    );
    await f.pay(o, "failed", randomUUID(), 100);
    assert.equal(
      (await f.control.getSubscription(f.admin, f.sub.id)).status,
      "active",
    );
    assert.equal((await f.store.balance(f.app))[0].balance, "2144");
  } finally {
    await f.store.close();
  }
});
test("P2 invalid signatures, amount mismatch and changed event payload never grant credits", async () => {
  const f = await setup();
  try {
    const o = await f.control.createOrder(
      f.admin,
      f.sub.id,
      f.integration.id,
      "order",
    );
    await assert.rejects(
      () =>
        f.control.payment(
          f.integration.id,
          Buffer.from("{}"),
          Math.floor(Date.now() / 1000).toString(),
          "v1=" + "0".repeat(64),
        ),
      /invalid_signature/,
    );
    await assert.rejects(
      () => f.pay(o, "succeeded", randomUUID(), 1, { amount_minor: "1" }),
      /payment_amount_mismatch/,
    );
    assert.equal((await f.store.balance(f.app)).length, 0);
    const id = randomUUID();
    await f.pay(o, "failed", id);
    await assert.rejects(
      () => f.pay(o, "succeeded", id, 2),
      /idempotency_conflict/,
    );
    assert.equal(
      (await f.control.getSubscription(f.admin, f.sub.id)).status,
      "past_due",
    );
    await f.pay(o, "succeeded", randomUUID(), 2);
    assert.equal((await f.store.balance(f.app))[0].balance, "2144");
  } finally {
    await f.store.close();
  }
});
test("P2 subscription credits fund real inference, exhaust safely, purchased credits remain independent", async () => {
  const f = await setup();
  try {
    const o = await f.control.createOrder(
      f.admin,
      f.sub.id,
      f.integration.id,
      "order",
    );
    await f.pay(o);
    const mock = new MockProvider(),
      router = new Router(f.store, { mock }, digestKey);
    const r = await router.infer(f.app, demoInput, "consume");
    assert.equal(r.billing_status, "settled");
    assert.equal((await f.store.balance(f.app))[0].balance, "2109");
    await assert.rejects(
      () => router.infer(f.app, demoInput, "exhausted"),
      /insufficient_balance/,
    );
    assert.equal(mock.calls, 1);
    await f.control.grantCredits(
      f.admin,
      {
        customer_id: "customer_demo",
        asset: f.plan.asset,
        amount: "1000",
        source_kind: "purchased",
        reason: "sandbox purchased credits",
      },
      "topup",
    );
    await router.infer(f.app, demoInput, "after-topup");
    assert.equal((await f.store.balance(f.app))[0].balance, "3074");
    const buckets = await f.control.buckets(f.app);
    assert.equal(
      buckets.find((b) => b.source_kind === "purchased")!.remaining,
      "1000",
    );
    assert.equal(
      buckets.find((b) => b.source_kind === "subscription")!.remaining,
      "2074",
    );
  } finally {
    await f.store.close();
  }
});
test("P2 plan Service scoping and explicit entitlement requirement cannot be bypassed", async () => {
  const f = await setup();
  try {
    await f.store.publish(f.admin, { ...demoService, id: "other" });
    await f.store.publish(f.admin, {
      ...demoService,
      id: "members-only",
      requires_subscription: true,
    });
    const o = await f.control.createOrder(
      f.admin,
      f.sub.id,
      f.integration.id,
      "order",
    );
    await f.pay(o);
    const mock = new MockProvider(),
      router = new Router(f.store, { mock }, digestKey);
    await assert.rejects(
      () =>
        router.infer(f.app, { ...demoInput, service: "other" }, "forbidden"),
      /scope_denied/,
    );
    const trusted = {
      ...f.app,
      auth_key_id: undefined,
      service_ids: undefined,
    };
    await assert.rejects(
      () =>
        router.infer(
          trusted,
          { ...demoInput, service: "other" },
          "restricted-credit",
        ),
      /insufficient_balance/,
    );
    await f.control.grantCredits(
      f.admin,
      {
        customer_id: "customer_demo",
        asset: f.plan.asset,
        amount: "10000",
        reason: "extra purchased credits",
      },
      "extra",
    );
    await assert.rejects(
      () =>
        router.infer(
          trusted,
          { ...demoInput, service: "members-only" },
          "no-entitlement",
        ),
      /subscription_required/,
    );
    assert.equal(mock.calls, 0);
  } finally {
    await f.store.close();
  }
});
test("P2 renewal grants fixed periods once, uses scheduled plan and ignores late old failures", async () => {
  const f = await setup();
  try {
    const first = await f.control.createOrder(
      f.admin,
      f.sub.id,
      f.integration.id,
      "first",
    );
    await f.pay(first);
    await f.control.publishPlan(
      f.admin,
      { ...f.plan, version: 2, credits: "4000" },
      "plan2",
    );
    await f.control.schedulePlan(
      f.admin,
      f.sub.id,
      { plan_id: f.plan.id, plan_version: 2 },
      "schedule",
    );
    await assert.rejects(
      () =>
        f.control.createOrder(f.admin, f.sub.id, f.integration.id, "too-early"),
      /period_already_paid/,
    );
    f.advance(30);
    await f.control.advancePeriods();
    assert.equal(
      (await f.control.getSubscription(f.admin, f.sub.id)).status,
      "past_due",
    );
    const renewal = await f.control.createOrder(
      f.admin,
      f.sub.id,
      f.integration.id,
      "renew",
    );
    assert.equal(renewal.cycle, 1);
    await f.pay(renewal, "failed", randomUUID(), 1);
    assert.equal(
      (await f.control.getSubscription(f.admin, f.sub.id)).status,
      "past_due",
    );
    await f.pay(renewal, "succeeded", randomUUID(), 2);
    assert.equal(
      (await f.control.getSubscription(f.admin, f.sub.id)).plan_version,
      2,
    );
    assert.equal(
      (await f.control.getSubscription(f.admin, f.sub.id)).status,
      "active",
    );
    await f.pay(first, "failed", randomUUID(), 999);
    assert.equal(
      (await f.control.getSubscription(f.admin, f.sub.id)).status,
      "active",
    );
    const rows = await f.store.pool.query(
      "SELECT granted::text FROM sr_buckets WHERE workspace=$1 ORDER BY expires_at",
      [f.admin.workspace],
    );
    assert.deepEqual(
      rows.rows.map((x) => x.granted),
      ["2144", "4000"],
    );
  } finally {
    await f.store.close();
  }
});
test("P2 cancel at period end preserves paid credits, rejects renewal, and late payment needs review", async () => {
  const f = await setup();
  try {
    const order = await f.control.createOrder(
      f.admin,
      f.sub.id,
      f.integration.id,
      "order",
    );
    await f.pay(order);
    await f.control.cancelSubscription(f.admin, f.sub.id, "cancel");
    assert.equal(
      (await f.control.getSubscription(f.admin, f.sub.id)).status,
      "active",
    );
    assert.equal((await f.store.balance(f.app))[0].balance, "2144");
    f.advance(30);
    await f.control.advancePeriods();
    assert.equal(
      (await f.control.getSubscription(f.admin, f.sub.id)).status,
      "cancelled",
    );
    await assert.rejects(
      () => f.control.createOrder(f.admin, f.sub.id, f.integration.id, "renew"),
      /subscription_cancelled/,
    );
    const second = await f.control.subscribe(
      f.admin,
      { customer_id: "customer_demo", plan_id: f.plan.id, plan_version: 1 },
      "second",
    );
    const late = await f.control.createOrder(
      f.admin,
      second.id,
      f.integration.id,
      "late",
    );
    await f.control.cancelSubscription(f.admin, second.id, "cancel-pending");
    assert.equal((await f.pay(late)).outcome, "review_required");
    const grants = await f.store.pool.query(
      "SELECT 1 FROM sr_entitlements WHERE workspace=$1 AND subscription=$2",
      [f.admin.workspace, second.id],
    );
    assert.equal(grants.rowCount, 0);
  } finally {
    await f.store.close();
  }
});
test("P2 tenant/environment read and audit isolation, read-only roles cannot mutate billing", async () => {
  const f = await setup(),
    g = await setup();
  try {
    await assert.rejects(
      () => g.control.getSubscription(g.admin, f.sub.id),
      /resource_not_found/,
    );
    await assert.rejects(
      () =>
        f.control.getSubscription(
          { ...f.admin, environment: "production" },
          f.sub.id,
        ),
      /invalid_key/,
    );
    await f.control.member(f.admin, { id: "viewer", role: "viewer" }, "viewer");
    const k = await f.control.issueKey(
      f.admin,
      {
        member_id: "viewer",
        customer_id: "_admin",
        scopes: ["billing:read", "operations:read"],
        expires_at: new Date(Date.now() + days).toISOString(),
      },
      "viewer-key",
    );
    const viewer = await f.control.authenticate(k.token!);
    await assert.rejects(
      () =>
        f.control.grantCredits(
          viewer,
          {
            customer_id: "customer_demo",
            asset: f.plan.asset,
            amount: "10",
            reason: "not authorized",
          },
          "denied",
        ),
      /scope_denied/,
    );
    const first = await f.control.auditLog(viewer, 2);
    assert.equal(first.data.length, 2);
    assert.ok(first.next_cursor);
    const next = await f.control.auditLog(viewer, 2, first.next_cursor);
    assert.ok(
      next.data.every((n: any) => !first.data.some((a: any) => a.id === n.id)),
    );
    const foreign = await g.control.auditLog(g.admin, 100);
    assert.ok(foreign.data.every((x: any) => x.resource !== f.sub.id));
    assert.ok(!JSON.stringify(first).includes(f.issued.token!));
  } finally {
    await f.store.close();
    await g.store.close();
  }
});
test("P2 full HTTP ingress authenticates DB keys and verifies signed payment without bearer shortcuts", async () => {
  const f = await setup();
  const server = createRouterServer(
    new Router(f.store, { mock: new MockProvider() }, digestKey),
    f.store,
    f.control,
  );
  try {
    server.listen(0, "127.0.0.1");
    await once(server, "listening");
    const url = "http://127.0.0.1:" + (server.address() as any).port;
    const request = (
      path: string,
      method = "GET",
      payload?: unknown,
      token = f.adminKey.token,
    ) =>
      fetch(url + path, {
        method,
        headers: {
          Authorization: "Bearer " + token,
          "Content-Type": "application/json",
          "Idempotency-Key": randomUUID(),
        },
        body: payload === undefined ? undefined : JSON.stringify(payload),
      });
    const orderResponse = await request(
      "/v1/admin/subscriptions/" + f.sub.id + "/payment-orders",
      "POST",
      { integration_id: f.integration.id },
    );
    assert.equal(orderResponse.status, 201);
    const order: any = await orderResponse.json();
    assert.equal(
      (
        await request(
          "/v1/admin/credits/grants",
          "POST",
          {
            customer_id: "customer_demo",
            asset: f.plan.asset,
            amount: "10000",
            reason: "bad app request",
          },
          f.issued.token!,
        )
      ).status,
      403,
    );
    const raw = JSON.stringify({
        id: randomUUID(),
        order_id: order.id,
        status: "succeeded",
        revision: 1,
        amount_minor: order.amount_minor,
        currency: order.currency,
      }),
      timestamp = Math.floor(Date.now() / 1000).toString();
    const signature =
      "v1=" +
      createHmac("sha256", f.integration.secret)
        .update(timestamp + "." + raw)
        .digest("hex");
    const endpoint =
      url + "/v1/integrations/payments/" + f.integration.id + "/events";
    assert.equal(
      (
        await fetch(endpoint, {
          method: "POST",
          headers: { Authorization: "Bearer " + f.adminKey.token },
          body: raw,
        })
      ).status,
      401,
    );
    assert.equal(
      (
        await fetch(endpoint, {
          method: "POST",
          headers: {
            "X-Payment-Timestamp": timestamp,
            "X-Payment-Signature": signature,
          },
          body: raw,
        })
      ).status,
      202,
    );
    const inference = await request(
      "/v1/inferences",
      "POST",
      demoInput,
      f.issued.token!,
    );
    assert.equal(inference.status, 200);
    assert.equal(((await inference.json()) as any).billing_status, "settled");
    const credits = await request(
      "/v1/credits",
      "GET",
      undefined,
      f.issued.token!,
    );
    assert.equal(credits.status, 200);
    assert.equal(((await credits.json()) as any).buckets[0].available, "2109");
    await f.control.revokeKey(f.admin, f.issued.id, "revoked");
    assert.equal(
      (await request("/v1/credits", "GET", undefined, f.issued.token!)).status,
      401,
    );
  } finally {
    await new Promise<void>((r) => server.close(() => r()));
    await f.store.close();
  }
});
test("P2 settlement follows promotion-before-purchased ordering for equal expiry", async () => {
  const f = await setup();
  try {
    await f.control.grantCredits(
      f.admin,
      {
        customer_id: "customer_demo",
        asset: f.plan.asset,
        amount: "10000",
        source_kind: "purchased",
        reason: "purchased test credits",
      },
      "purchase",
    );
    await f.control.grantCredits(
      f.admin,
      {
        customer_id: "customer_demo",
        asset: f.plan.asset,
        amount: "10",
        source_kind: "promotion",
        reason: "promotion test credits",
      },
      "promotion",
    );
    // Force opposite UUID order so the priority assertion is deterministic.
    await f.store.pool.query(
      "UPDATE sr_buckets SET id=CASE WHEN source_kind='promotion' THEN 'z-promotion' ELSE 'a-purchased' END WHERE workspace=$1",
      [f.admin.workspace],
    );
    const router = new Router(f.store, { mock: new MockProvider() }, digestKey);
    await router.infer(f.app, demoInput, "priority");
    const b = await f.control.buckets(f.app);
    assert.equal(b.find((x) => x.source_kind === "promotion")!.remaining, "0");
    assert.equal(
      b.find((x) => x.source_kind === "purchased")!.remaining,
      "9975",
    );
  } finally {
    await f.store.close();
  }
});
test("P2 revoked key between reservation and dispatch does not call provider or strand a hold", async () => {
  const f = await setup();
  try {
    await f.control.grantCredits(
      f.admin,
      {
        customer_id: "customer_demo",
        asset: f.plan.asset,
        amount: "10000",
        reason: "revocation race fixture",
      },
      "grant",
    );
    const mock = new MockProvider(),
      router = new Router(f.store, { mock }, digestKey);
    let id = "";
    await assert.rejects(
      () =>
        router.infer(f.app, demoInput, "revoke-race", async (e) => {
          if (e.type === "request.started") {
            id = e.request_id;
            await f.control.revokeKey(
              f.admin,
              f.issued.id,
              "revoke-during-request",
            );
          }
        }),
      /invalid_key/,
    );
    assert.equal(mock.calls, 0);
    assert.equal((await f.store.get(f.app, id)).billing, "released");
    assert.equal((await f.store.balance(f.app))[0].held, "0");
  } finally {
    await f.store.close();
  }
});
test("P2 expired subscription credits do not roll over; standalone purchased credits survive", async () => {
  const f = await setup();
  try {
    f.advance(-31);
    const old = await f.control.subscribe(
      f.admin,
      { customer_id: "customer_demo", plan_id: f.plan.id, plan_version: 1 },
      "historical",
    );
    const order = await f.control.createOrder(
      f.admin,
      old.id,
      f.integration.id,
      "historical-order",
    );
    await f.pay(order);
    f.advance(31);
    await f.control.advancePeriods();
    await f.control.grantCredits(
      f.admin,
      {
        customer_id: "customer_demo",
        asset: f.plan.asset,
        amount: "3000",
        source_kind: "purchased",
        reason: "independent purchased credits",
      },
      "purchase",
    );
    assert.equal((await f.store.balance(f.app))[0].balance, "3000");
    assert.equal(
      (await f.control.getSubscription(f.admin, old.id)).status,
      "past_due",
    );
    const router = new Router(f.store, { mock: new MockProvider() }, digestKey);
    assert.equal(
      (await router.infer(f.app, demoInput, "purchased-after-expiry"))
        .billing_status,
      "settled",
    );
    assert.equal((await f.store.balance(f.app))[0].balance, "2965");
  } finally {
    await f.store.close();
  }
});
test("P2 integration identity cannot be rebound across tenants and signatures expire", async () => {
  const f = await setup(),
    g = await setup();
  try {
    const o = await f.control.createOrder(
      f.admin,
      f.sub.id,
      f.integration.id,
      "order",
    );
    await f.pay(o);
    const spoof = new ControlPlane(g.store, [
      {
        ...f.integration,
        workspace: g.admin.workspace,
        secret: g.integration.secret,
      },
    ]);
    const go = await spoof.createOrder(
      g.admin,
      g.sub.id,
      f.integration.id,
      "other-order",
    );
    const raw = Buffer.from(
        JSON.stringify({
          id: randomUUID(),
          order_id: go.id,
          status: "succeeded",
          revision: 1,
          amount_minor: go.amount_minor,
          currency: go.currency,
        }),
      ),
      timestamp = Math.floor(Date.now() / 1000).toString();
    const signature =
      "v1=" +
      createHmac("sha256", g.integration.secret)
        .update(timestamp + ".")
        .update(raw)
        .digest("hex");
    await assert.rejects(
      () => spoof.payment(f.integration.id, raw, timestamp, signature),
      /integration_binding_conflict/,
    );
    await assert.rejects(
      () =>
        spoof.payment(
          f.integration.id,
          raw,
          String(Number(timestamp) - 301),
          signature,
        ),
      /invalid_signature/,
    );
    assert.equal((await g.store.balance(g.app)).length, 0);
  } finally {
    await f.store.close();
    await g.store.close();
  }
});
test("P2 bootstrap uses caller-held key without persisting plaintext; restricted service admin cannot publish elsewhere", async () => {
  const store = new PostgresStore(
    process.env.DATABASE_URL ?? LOCAL_DATABASE_URL,
  );
  await store.migrate();
  const control = new ControlPlane(store),
    workspace = "bootstrap-" + randomUUID(),
    token = "sr_" + randomBytes(32).toString("hex");
  try {
    await control.bootstrap(workspace, "test", "owner", token);
    const owner = await control.authenticate(token);
    await store.publish(owner, demoService);
    await control.member(
      owner,
      { id: "developer", role: "service_admin" },
      "member",
    );
    const key = await control.issueKey(
      owner,
      {
        member_id: "developer",
        customer_id: "_admin",
        scopes: ["service:write"],
        service_ids: ["chat"],
        expires_at: new Date(Date.now() + days).toISOString(),
      },
      "key",
    );
    const developer = await control.authenticate(key.token!);
    await assert.rejects(
      () => store.publish(developer, { ...demoService, id: "forbidden" }),
      /scope_denied/,
    );
    await assert.rejects(
      () => control.bootstrap(workspace, "test", "different-owner"),
      /already_bootstrapped/,
    );
    const row = await store.pool.query(
      "SELECT row_to_json(k) AS record FROM sr_keys k WHERE workspace=$1",
      [workspace],
    );
    assert.ok(!JSON.stringify(row.rows).includes(token));
    const commands = await store.pool.query(
      "SELECT result FROM sr_commands WHERE workspace=$1 AND operation=$2",
      [workspace, "key.issue"],
    );
    assert.equal(commands.rows[0].result.token, null);
  } finally {
    await store.close();
  }
});
