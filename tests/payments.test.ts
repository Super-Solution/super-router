import { once } from "node:events";
import { Router, MockProvider } from "@superrouter/core";
import { createRouterServer } from "../apps/server/server.js";
import test from "node:test";
import assert from "node:assert/strict";
import { createHmac, randomUUID } from "node:crypto";
import {
  StripePayments,
  OenPayments,
  EpayV1Payments,
  epaySign,
  verifyHmac,
  type PaymentAdapter,
} from "@superrouter/payments";
import {
  PostgresStore,
  ControlPlane,
  LOCAL_DATABASE_URL,
} from "@superrouter/storage-postgres";
import { demoService } from "../scripts/demo-config.js";
const sign = (raw: Buffer, secret = "secret") => {
  const t = Math.floor(Date.now() / 1000);
  return `t=${t},v1=${createHmac("sha256", secret)
    .update(t + ".")
    .update(raw)
    .digest("hex")}`;
};
const transport = (fn: (url: string, init: RequestInit) => unknown) =>
  (async (url, init) =>
    new Response(JSON.stringify(fn(String(url), init ?? {})), {
      status: 200,
    })) as typeof fetch;
const order = { id: "order-1", amount_minor: "120000", currency: "TWD" };
test("HMAC rejects tampered bodies, stale timestamps and missing signatures", () => {
  const raw = Buffer.from('{"id":1}');
  verifyHmac(raw, sign(raw), "secret");
  assert.throws(() => verifyHmac(Buffer.from("{}"), sign(raw), "secret"));
  assert.throws(() => verifyHmac(raw, "t=1000000000,v1=bad", "secret"));
  assert.throws(() => verifyHmac(raw, "", "secret"));
});
test("Stripe verifies merchant, checkout mode, signed events and authoritative payment status", async () => {
  let paid = false;
  const stripe = new StripePayments(
    {
      secretKey: "fixture",
      webhookSecret: "secret",
      accountId: "acct_fixture",
      live: false,
      successUrl: "https://shop.example/success",
      cancelUrl: "https://shop.example/cancel",
    },
    transport((url, init) => {
      if (url.endsWith("/account")) return { id: "acct_fixture" };
      if (init.method === "POST") {
        const b = new URLSearchParams(String(init.body));
        assert.equal(b.get("mode"), "payment");
        assert.equal(b.get("line_items[0][price_data][unit_amount]"), "120000");
        return {
          id: "cs_fixture",
          url: "https://checkout.stripe.com/c/pay/fixture",
          livemode: false,
        };
      }
      return {
        id: "cs_fixture",
        livemode: false,
        client_reference_id: order.id,
        amount_total: 120000,
        currency: "twd",
        payment_status: paid ? "paid" : "unpaid",
      };
    }),
  );
  assert.equal((await stripe.create(order)).reference, "cs_fixture");
  assert.equal(await stripe.lookup("cs_fixture"), null);
  paid = true;
  assert.equal((await stripe.lookup("cs_fixture"))?.amount_minor, "120000");
  const raw = Buffer.from(
    JSON.stringify({
      type: "checkout.session.completed",
      livemode: false,
      data: { object: { id: "cs_fixture" } },
    }),
  );
  assert.equal(
    stripe.notification(raw, { "stripe-signature": sign(raw) }),
    "cs_fixture",
  );
  assert.throws(() => stripe.notification(raw, {}));
});
test("OEN converts ISO TWD minor units to whole dollars and queries internal transaction ID", async () => {
  const ref = "a".repeat(27);
  let status = "charging";
  const oen = new OenPayments(
    {
      token: "fixture",
      webhookSecret: "secret",
      merchantId: "demo",
      live: false,
      successUrl: "https://shop.example/success",
      failureUrl: "https://shop.example/failure",
    },
    transport((url, init) => {
      if (init.method === "POST") {
        const b = JSON.parse(String(init.body));
        assert.equal(b.amount, 1200);
        assert.equal(b.productDetails[0].unitPrice, 1200);
        return { code: "S0000", data: { id: ref } };
      }
      assert.ok(url.endsWith("/transactions/" + ref));
      return {
        code: "S0000",
        data: { transactionId: ref, orderId: order.id, amount: 1200, status },
      };
    }),
  );
  assert.equal(
    (await oen.create(order)).url,
    `https://demo.testing.oen.tw/checkout/${ref}`,
  );
  await assert.rejects(oen.create({ ...order, amount_minor: "101" }));
  assert.equal(await oen.lookup(ref), null);
  status = "charged";
  assert.equal((await oen.lookup(ref))?.amount_minor, "120000");
  const raw = Buffer.from(
    JSON.stringify({ id: ref, purpose: "charge", merchantId: "demo" }),
  );
  assert.equal(oen.notification(raw, { "oenpay-signature": sign(raw) }), ref);
  assert.throws(() => oen.notification(raw, {}));
});
test("Epay V1 signs sorted unencoded fields, rejects duplicate parameters and queries payment", async () => {
  const epay = new EpayV1Payments(
    {
      endpoint: "https://pay.example/",
      pid: "1001",
      key: "fixture",
      notifyUrl: "https://api.example/notify",
      returnUrl: "https://shop.example/return",
    },
    transport(() => ({
      code: 1,
      pid: 1001,
      out_trade_no: order.id,
      status: 1,
      money: "12.34",
    })),
  );
  const checkout = await epay.create({
    ...order,
    amount_minor: "1234",
    currency: "CNY",
  });
  const p = new URL(checkout.url).searchParams;
  assert.equal(p.get("money"), "12.34");
  assert.equal(p.get("sign"), epaySign(Object.fromEntries(p), "fixture"));
  const data = {
    pid: "1001",
    out_trade_no: order.id,
    trade_no: "trade1",
    money: "12.34",
    trade_status: "TRADE_SUCCESS",
    sign_type: "MD5",
  };
  const raw = Buffer.from(
    new URLSearchParams({
      ...data,
      sign: epaySign(data, "fixture"),
    }).toString(),
  );
  assert.equal(epay.notification(raw), order.id);
  assert.throws(() =>
    epay.notification(Buffer.from(raw.toString() + "&pid=1001")),
  );
  assert.equal((await epay.lookup(order.id))?.amount_minor, "1234");
});
async function fixture(fail = false) {
  const store = new PostgresStore(
    process.env.DATABASE_URL ?? LOCAL_DATABASE_URL,
  );
  await store.migrate();
  const workspace = "payments-" + randomUUID();
  let calls = 0;
  let amount = "999";
  let orderId = "";
  const adapter: PaymentAdapter = {
    identity: "fixture:merchant:test",
    capabilities: {
      hosted_checkout: true,
      automatic_recurring: false,
      refunds: false,
      portal: false,
    },
    acknowledgement: "success",
    async create(o) {
      calls++;
      orderId = o.id;
      if (fail) throw new Error("network timeout");
      return { reference: "ref-" + o.id, url: "https://pay.example/checkout" };
    },
    notification(raw) {
      return raw.toString();
    },
    async lookup(reference) {
      return {
        reference,
        order_id: orderId,
        amount_minor: amount,
        currency: "USD",
      };
    },
  };
  const integration = {
    id: randomUUID(),
    workspace,
    environment: "test",
    mode: "hosted" as const,
    adapter,
  };
  const control = new ControlPlane(store, [integration]);
  const key = await control.bootstrap(workspace, "test");
  const p = await control.authenticate(key.token);
  await store.publish(p, demoService);
  await control.customer(
    p,
    { id: "customer", external_id: "external" },
    "customer",
  );
  await control.publishPlan(
    p,
    {
      id: "plan",
      version: 1,
      period_days: 30,
      asset: demoService.price.asset,
      credits: "1000",
      service_ids: ["chat"],
      amount_minor: "999",
      currency: "USD",
    },
    "plan",
  );
  const sub = await control.subscribe(
    p,
    { customer_id: "customer", plan_id: "plan", plan_version: 1 },
    "sub",
  );
  const o = await control.createOrder(p, sub.id, integration.id, "order");
  return {
    store,
    control,
    p,
    o,
    integration,
    calls: () => calls,
    token: key.token,
    setAmount: (value: string) => {
      amount = value;
    },
  };
}
test("checkout is single dispatch, tenant scoped, amount checked, duplicate callbacks grant once", async () => {
  const f = await fixture();
  try {
    const c = await f.control.checkout(f.p, f.o.id);
    assert.deepEqual(await f.control.checkout(f.p, f.o.id), c);
    assert.equal(f.calls(), 1);
    await assert.rejects(
      f.control.checkout({ ...f.p, workspace: "other" }, f.o.id),
    );
    f.setAmount("1000");
    await assert.rejects(f.control.reconcilePayment(f.p, f.o.id));
    f.setAmount("999");
    await Promise.all([
      f.control.hostedNotification(
        f.integration.id,
        Buffer.from(c.reference),
        {},
      ),
      f.control.reconcilePayment(f.p, f.o.id),
    ]);
    const buckets = await f.control.buckets(f.p, "customer");
    assert.equal(buckets.length, 1);
    assert.equal(buckets[0].available, "1000");
    await assert.rejects(
      f.control.payment(f.integration.id, Buffer.from("{}"), "", ""),
    );
  } finally {
    await f.store.close();
  }
});
test("uncertain checkout creation is fenced from repeated external requests", async () => {
  const f = await fixture(true);
  try {
    await assert.rejects(f.control.checkout(f.p, f.o.id));
    await assert.rejects(f.control.checkout(f.p, f.o.id));
    assert.equal(f.calls(), 1);
    f.setAmount("998");
    await assert.rejects(f.control.reconcilePayment(f.p, f.o.id, "recovered"));
    f.setAmount("999");
    assert.equal(
      (await f.control.reconcilePayment(f.p, f.o.id, "recovered")).outcome,
      "granted",
    );
    assert.equal(
      (await f.control.reconcilePayment(f.p, f.o.id)).outcome,
      "granted",
    );
    assert.equal((await f.control.buckets(f.p, "customer")).length, 1);
  } finally {
    await f.store.close();
  }
});

test("late confirmed payment enters review without granting expired credits", async () => {
  const f = await fixture();
  try {
    await f.control.checkout(f.p, f.o.id);
    await f.store.pool.query(
      "UPDATE sr_payment_orders SET period_start=now()-interval '30 days',period_end=now()-interval '1 second' WHERE workspace=$1 AND environment=$2 AND id=$3",
      [f.p.workspace, f.p.environment, f.o.id],
    );
    assert.equal(
      (await f.control.reconcilePayment(f.p, f.o.id)).outcome,
      "review_required",
    );
    assert.equal((await f.control.buckets(f.p, "customer")).length, 0);
  } finally {
    await f.store.close();
  }
});

test("hosted HTTP checkout requires billing authentication; GET callback acknowledges only after settlement", async () => {
  const f = await fixture();
  const server = createRouterServer(
    new Router(
      f.store,
      { mock: new MockProvider() },
      "fixture-digest-key-at-least-32-characters",
    ),
    f.store,
    f.control,
  );
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  const address = server.address() as { port: number };
  const base = `http://127.0.0.1:${address.port}`;
  try {
    const path = base + "/v1/admin/payment-orders/" + f.o.id + "/checkout";
    assert.equal((await fetch(path, { method: "POST" })).status, 401);
    const response = await fetch(path, {
      method: "POST",
      headers: {
        authorization: "Bearer " + f.token,
        "Idempotency-Key": "checkout",
      },
    });
    assert.equal(response.status, 200);
    const checkout = (await response.json()) as { reference: string };
    const notify =
      base +
      "/v1/integrations/payments/" +
      f.integration.id +
      "/notify?" +
      checkout.reference;
    const ack = await fetch(notify);
    assert.equal(ack.status, 200);
    assert.equal(await ack.text(), "success");
    assert.equal((await f.control.buckets(f.p, "customer")).length, 1);
    assert.equal((await fetch(notify)).status, 200);
    assert.equal((await f.control.buckets(f.p, "customer")).length, 1);
  } finally {
    await new Promise<void>((resolve) => server.close(() => resolve()));
    await f.store.close();
  }
});
