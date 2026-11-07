import { randomUUID, randomBytes, createHmac } from "node:crypto";
import { once } from "node:events";
import assert from "node:assert/strict";
import {
  PostgresStore,
  ControlPlane,
  LOCAL_DATABASE_URL,
  type PaymentIntegration,
} from "@supersolutions/super-router-storage-postgres";
import { Router, MockProvider } from "@supersolutions/super-router-core";
import { SuperRouterClient } from "@supersolutions/super-router-sdk";
import { createRouterServer } from "@supersolutions/super-router-server";
import {
  demoService,
  demoInput,
  digestKey,
} from "../../scripts/demo-config.js";
const store = new PostgresStore(process.env.DATABASE_URL ?? LOCAL_DATABASE_URL);
await store.migrate();
const workspace = "subscription-demo-" + randomUUID();
const integration: PaymentIntegration = {
  id: "sandbox-" + randomUUID(),
  workspace,
  environment: "test",
  secret: randomBytes(32).toString("hex"),
  mode: "sandbox",
};
const control = new ControlPlane(store, [integration]);
const owner = await control.bootstrap(workspace, "test");
const ownerPrincipal = await control.authenticate(owner.token);
await store.publish(ownerPrincipal, demoService);
const mock = new MockProvider();
const server = createRouterServer(
  new Router(store, { mock }, digestKey),
  store,
  control,
);
try {
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  const base =
    "http://127.0.0.1:" + (server.address() as { port: number }).port;
  const admin = new SuperRouterClient(base, owner.token);
  console.log(
    "SANDBOX payment + MOCK model; real PostgreSQL, persistent API-key authorization, HTTP and ledger. No real money or model calls.",
  );
  await admin.createCustomer(
    { id: "customer_demo", external_id: "example-customer" },
    "customer",
  );
  await admin.createMember({ id: "app", role: "app", active: true }, "member");
  const key = await admin.issueKey(
    {
      member_id: "app",
      customer_id: "customer_demo",
      scopes: ["invoke", "credits:read"],
      service_ids: ["chat"],
      expires_at: new Date(Date.now() + 86400000).toISOString(),
    },
    "app-key",
  );
  assert.ok(key.token);
  const app = new SuperRouterClient(base, key.token);
  await admin.publishPlan(
    {
      id: "basic",
      version: 1,
      period_days: 30,
      asset: demoService.price.asset,
      credits: "2144",
      service_ids: ["chat"],
      amount_minor: "999",
      currency: "USD",
    },
    "plan",
  );
  const sub = await admin.subscribe(
    { customer_id: "customer_demo", plan_id: "basic", plan_version: 1 },
    "subscription",
  );
  const order = await admin.paymentOrder(sub.id, integration.id, "order");
  const payload = JSON.stringify({
    id: randomUUID(),
    order_id: order.id,
    revision: 1,
    status: "succeeded",
    amount_minor: order.amount_minor,
    currency: order.currency,
  });
  const timestamp = Math.floor(Date.now() / 1000).toString();
  const signature =
    "v1=" +
    createHmac("sha256", integration.secret)
      .update(timestamp + "." + payload)
      .digest("hex");
  const deliver = () =>
    fetch(base + "/v1/integrations/payments/" + integration.id + "/events", {
      method: "POST",
      headers: {
        "X-Payment-Timestamp": timestamp,
        "X-Payment-Signature": signature,
      },
      body: payload,
    });
  assert.equal((await deliver()).status, 202);
  assert.equal((await deliver()).status, 202);
  console.log(
    "Payment delivered twice: granted once, credits=",
    ((await app.credits()) as any).buckets[0].available,
  );
  const result = (await app.infer(demoInput, "first")) as any;
  assert.equal(result.billing_status, "settled");
  console.log(
    "First inference:",
    result.execution_status,
    result.billing_status,
    "remaining=",
    ((await app.credits()) as any).buckets[0].available,
  );
  await assert.rejects(
    () => app.infer(demoInput, "exhausted"),
    /insufficient_balance/,
  );
  console.log(
    "Second call rejected before dispatch: remaining credits cannot cover conservative 2144-atom reservation.",
  );
  await assert.rejects(
    () =>
      app.grantCredits(
        {
          customer_id: "customer_demo",
          asset: demoService.price.asset,
          amount: "1000",
          source_kind: "promotion",
          reason: "unauthorized app grant",
        },
        "bad-grant",
      ),
    /scope_denied/,
  );
  console.log("App cannot grant itself credits.");
  await admin.grantCredits(
    {
      customer_id: "customer_demo",
      asset: demoService.price.asset,
      amount: "1000",
      source_kind: "purchased",
      reason: "sandbox manual top-up",
    },
    "topup",
  );
  await app.infer(demoInput, "after-topup");
  console.log("After authorized top-up + inference:", await app.credits());
  await admin.cancelSubscription(sub.id, "cancel");
  console.log(
    "Cancelled next renewal; current paid period stays active:",
    await admin.subscription(sub.id),
  );
  await admin.revokeKey(key.id, "revoke");
  await assert.rejects(() => app.credits(), /invalid_key/);
  assert.equal(mock.calls, 2);
  console.log("Revoked key rejected. Provider calls:", mock.calls);
} finally {
  await new Promise<void>((r) => server.close(() => r()));
  await store.close();
}
