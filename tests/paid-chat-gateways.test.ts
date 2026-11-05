import { request as httpRequest } from "node:http";
import test from "node:test";
import assert from "node:assert/strict";
import { randomUUID, createHmac, randomBytes } from "node:crypto";
import { startPaidChat } from "../examples/paid-chat/server.js";
import { buildGateways } from "../examples/paid-chat/gateways.js";
import { services } from "../examples/paid-chat/config.js";

// These exercise each real gateway adapter against the explicit local provider fixture,
// the customer-facing HTTP BFF, authoritative lookup, and the PostgreSQL ledger.
for (const [index, method] of (["stripe", "oen", "epay"] as const).entries()) {
  test(`${method}: customer checkout, verified notification, one grant, inference and retry`, async () => {
    const app = await startPaidChat({
      port: 4351 + index * 2,
      checkoutPort: 4352 + index * 2,
      workspace: "gateway-app-" + method + "-" + randomUUID(),
      paymentMode: "fixture",
      quiet: true,
    });
    let cookie = "";
    const request = async (
      path: string,
      body?: unknown,
      options: { cookie?: string; key?: string } = {},
    ) => {
      const res = await fetch(app.origin + path, {
        method: body === undefined ? "GET" : "POST",
        headers: {
          Cookie: options.cookie ?? cookie,
          ...(body === undefined
            ? {}
            : {
                "Content-Type": "application/json",
                Origin: app.origin,
                "Idempotency-Key": options.key ?? randomUUID(),
              }),
        },
        ...(body === undefined ? {} : { body: JSON.stringify(body) }),
      });
      return { res, value: await res.json() };
    };
    let orderId = "";
    try {
      const gateway = app.gateways[method];
      assert.ok(gateway);
      const fixture = gateway.fixture;
      assert.ok(fixture, "fixture is explicit, never a live merchant");
      const notify = async (
        overrides: Parameters<typeof fixture.notification>[1] = {},
      ) => {
        const message = fixture.notification(orderId, overrides);
        const endpoint = `${app.origin}/api/payments/${method}/notify`;
        return method === "epay"
          ? fetch(endpoint + "?" + message.raw.toString(), { method: "GET" })
          : fetch(endpoint, {
              method: "POST",
              headers: message.headers,
              body: new Uint8Array(message.raw),
            });
      };
      const publicCatalog = await request("/api/catalog");
      assert.equal(publicCatalog.res.status, 200);
      assert.ok(
        publicCatalog.value.payment_methods.some(
          (m: { id: string }) => m.id === method,
        ),
      );
      const offer = publicCatalog.value.offers.find(
        (o: { id: string }) =>
          o.id === (method === "epay" ? "starter-cny-v1" : "starter-v1"),
      );
      assert.ok(offer);
      assert.equal(offer.plan.currency, method === "epay" ? "CNY" : "TWD");
      const session = await request("/api/session", { demo: true });
      assert.equal(session.res.status, 200);
      cookie = session.res.headers.get("set-cookie")!.split(";")[0]!;
      assert.equal((await request("/api/billing/me")).value.buckets.length, 0);
      const checkoutKey = randomUUID();
      const checkout = await request(
        "/api/billing/checkout",
        { offer_id: offer.id, payment_method: method },
        { key: checkoutKey },
      );
      assert.equal(checkout.res.status, 200, JSON.stringify(checkout.value));
      orderId = checkout.value.order_id;
      const redirect = new URL(checkout.value.redirect_url);
      assert.equal(redirect.origin, app.checkoutOrigin);
      assert.equal(redirect.searchParams.get("provider"), method);
      assert.ok(redirect.searchParams.get("ticket"));
      const replay = await request(
        "/api/billing/checkout",
        { offer_id: offer.id, payment_method: method },
        { key: checkoutKey },
      );
      assert.deepEqual(replay.value, checkout.value);
      const rows = await app.store.pool.query(
        "SELECT count(*)::int AS count FROM sr_checkouts WHERE workspace=$1 AND environment=$2 AND order_id=$3",
        [app.host.workspace, app.host.environment, orderId],
      );
      assert.equal(
        rows.rows[0].count,
        1,
        "checkout creation is single dispatch",
      );

      // Returning from a hosted page, or a signed claim while provider lookup says unpaid,
      // cannot mint credits. Callback bodies are not the source of payment truth.
      assert.equal(
        (
          await fetch(`${app.origin}/?order=${orderId}`, {
            headers: { Cookie: cookie },
          })
        ).status,
        200,
      );
      assert.equal(
        (await request("/api/billing/orders/" + orderId)).value
          .fulfillment_status,
        "pending",
      );
      assert.equal((await notify({ badSignature: true })).status, 401);
      assert.equal((await notify({ amount_minor: "1" })).status, 200);
      assert.equal((await request("/api/billing/me")).value.buckets.length, 0);
      const page = await fetch(redirect, { headers: { Cookie: cookie } });
      assert.equal(page.status, 200);
      const html = await page.text();
      assert.match(html, /fixture|模擬|沙盒/i);
      assert.ok(
        html.includes(method) ||
          html
            .toLowerCase()
            .includes(
              method === "oen" ? "應援" : method === "epay" ? "易支付" : method,
            ),
      );

      // Another customer cannot inspect or complete this customer's provider checkout.
      const secondCustomer = "other-" + method;
      await app.control.customer(
        app.host,
        { id: secondCustomer, external_id: secondCustomer },
        "customer:" + secondCustomer,
      );
      const key = await app.control.issueKey(
        app.host,
        {
          member_id: "paid-app",
          customer_id: secondCustomer,
          scopes: ["invoke", "credits:read"],
          service_ids: Object.values(services).map((s) => s.id),
          expires_at: new Date(Date.now() + 3600000).toISOString(),
        },
        "key:" + secondCustomer,
      );
      const otherSession = await request(
        "/api/session",
        { key: key.token },
        { cookie: "" },
      );
      const otherCookie = otherSession.res.headers
        .get("set-cookie")!
        .split(";")[0]!;
      assert.equal(
        (
          await request("/api/billing/orders/" + orderId, undefined, {
            cookie: otherCookie,
          })
        ).res.status,
        404,
      );
      const complete = async (sessionCookie = cookie) =>
        fetch(app.checkoutOrigin + "/complete", {
          method: "POST",
          headers: {
            "Content-Type": "application/json",
            Origin: app.checkoutOrigin,
            Cookie: sessionCookie,
          },
          body: JSON.stringify({
            ticket: redirect.searchParams.get("ticket"),
            provider: method,
          }),
        });
      assert.equal((await complete(otherCookie)).status, 404);
      assert.equal((await request("/api/billing/me")).value.buckets.length, 0);
      fixture.markPaid(orderId, {
        amount_minor: (BigInt(offer.plan.amount_minor) + 100n).toString(),
      });
      const wrongReceipt = await notify();
      assert.equal(wrongReceipt.status, 422);
      assert.equal(
        (await wrongReceipt.json()).error,
        "payment_amount_mismatch",
      );
      assert.equal((await request("/api/billing/me")).value.buckets.length, 0);
      assert.equal((await complete()).status, 200);
      const settled = await request("/api/billing/me");
      assert.equal(settled.value.buckets.length, 1);
      assert.equal(settled.value.buckets[0].available, offer.plan.credits);
      const order = await request("/api/billing/orders/" + orderId);
      assert.equal(order.value.payment_status, "succeeded");
      assert.equal(order.value.fulfillment_status, "fulfilled");
      assert.equal(order.value.currency, offer.plan.currency);
      assert.equal(order.value.amount_minor, offer.plan.amount_minor);
      for (let attempt = 0; attempt < 2; attempt++) {
        const duplicate = await notify();
        assert.equal(duplicate.status, 200);
        assert.equal(
          await duplicate.text(),
          method === "epay" ? "success" : "ok",
        );
      }
      assert.ok(
        fixture.requests.some((r) =>
          method === "stripe"
            ? r.path.startsWith("/v1/checkout/sessions/")
            : method === "oen"
              ? r.path.startsWith("/transactions/")
              : r.path === "/api.php",
        ),
        "real provider adapter performs authoritative lookup",
      );
      assert.equal(
        (await request("/api/billing/me")).value.buckets[0].available,
        offer.plan.credits,
      );
      const grants = await app.store.pool.query(
        "SELECT amount::text,asset FROM sr_journal WHERE workspace=$1 AND environment=$2 AND reason='grant'",
        [app.host.workspace, app.host.environment],
      );
      assert.equal(grants.rowCount, 1);
      assert.equal(grants.rows[0].amount, offer.plan.credits);
      assert.equal(grants.rows[0].asset, offer.plan.asset);
      const active = await request("/api/catalog");
      const inferKey = randomUUID();
      const input = {
        mode: "chat",
        prompt: "After verified payment",
        expected_service_version: active.value.services.chat.version,
      };
      const infer = await request("/api/infer", input, { key: inferKey });
      assert.equal(infer.res.status, 200, JSON.stringify(infer.value));
      assert.equal(infer.value.billing_status, "settled");
      const charged = infer.value.billing.charges.find(
        (c: { kind: string }) => c.kind === "customer_sale",
      ).atoms;
      assert.equal(charged, "35000");
      assert.equal(
        (await request("/api/billing/me")).value.buckets[0].available,
        (BigInt(offer.plan.credits) - BigInt(charged)).toString(),
      );
      const inferReplay = await request("/api/infer", input, { key: inferKey });
      assert.equal(inferReplay.value.id, infer.value.id);
      assert.equal(inferReplay.value.replayed, true);
      assert.equal(app.mock.calls, 1);
      const debits = await app.store.pool.query(
        "SELECT amount::text FROM sr_settlements WHERE workspace=$1 AND environment=$2 AND kind='customer_sale'",
        [app.host.workspace, app.host.environment],
      );
      assert.equal(debits.rowCount, 1);
      assert.equal(debits.rows[0].amount, charged);
    } finally {
      await app.close();
    }
  });
}

test("merchant factory uses only enabled credentials and fences Stripe test/live notifications without network", () => {
  const config = {
    mode: "merchant" as const,
    checkoutOrigin: "http://127.0.0.1:4362",
    workspace: "merchant-factory-fixture",
    secret: "local-fixture-secret",
    publicOrigin: "https://merchant.fixture.invalid",
    env: {
      PAID_STRIPE_MODE: "test",
      PAID_STRIPE_SECRET_KEY: "synthetic-not-a-credential",
      PAID_STRIPE_WEBHOOK_SECRET: "synthetic-webhook",
      PAID_STRIPE_ACCOUNT_ID: "acct_fixture",
    },
  };
  const testing = buildGateways(config);
  assert.deepEqual(Object.keys(testing), ["stripe"]);
  assert.equal(testing.stripe!.fixture, undefined);
  assert.equal(testing.stripe!.mode, "test");
  const live = buildGateways({
    ...config,
    env: { ...config.env, PAID_STRIPE_MODE: "live" },
  });
  assert.notEqual(
    testing.stripe!.adapter.identity,
    live.stripe!.adapter.identity,
  );
  const signed = (livemode: boolean) => {
    const raw = Buffer.from(
      JSON.stringify({
        type: "checkout.session.completed",
        livemode,
        data: { object: { id: "cs_fixture" } },
      }),
    );
    const timestamp = Math.floor(Date.now() / 1000);
    const signature = createHmac(
      "sha256",
      config.env.PAID_STRIPE_WEBHOOK_SECRET,
    )
      .update(timestamp + ".")
      .update(raw)
      .digest("hex");
    return {
      raw,
      headers: { "stripe-signature": `t=${timestamp},v1=${signature}` },
    };
  };
  const event = signed(false);
  assert.equal(
    testing.stripe!.adapter.notification(event.raw, event.headers),
    "cs_fixture",
  );
  assert.equal(
    live.stripe!.adapter.notification(event.raw, event.headers),
    "",
    "test notification cannot settle a live integration",
  );
  const liveEvent = signed(true);
  assert.equal(
    testing.stripe!.adapter.notification(liveEvent.raw, liveEvent.headers),
    "",
  );
  assert.equal(
    live.stripe!.adapter.notification(liveEvent.raw, liveEvent.headers),
    "cs_fixture",
  );
  assert.throws(
    () => buildGateways({ ...config, env: { PAID_STRIPE_MODE: "test" } }),
    /Missing merchant configuration/,
  );
  assert.throws(
    () => buildGateways({ ...config, env: {} }),
    /explicitly enabled gateway/,
  );
  assert.throws(
    () =>
      buildGateways({
        ...config,
        env: {
          ...config.env,
          PAID_OEN_MODE: "live",
          PAID_OEN_TOKEN: "synthetic-no-network",
          PAID_OEN_WEBHOOK_SECRET: "synthetic-oen",
          PAID_OEN_MERCHANT_ID: "synthetic",
        },
      }),
    /Test and live merchants require separate/,
  );
});

test("merchant composition exposes enabled gateway only, disables demo and rejects untrusted public notify without external calls", async () => {
  const app = await startPaidChat({
    port: 4361,
    checkoutPort: 4362,
    workspace: "merchant-http-" + randomUUID(),
    paymentMode: "merchant",
    publicOrigin: "https://merchant.fixture.invalid",
    quiet: true,
    gatewayEnv: {
      PAID_SERVER_SECRET: "private-synthetic-fixture-only-no-network",
      PAID_HOST_KEY: "sr_" + randomBytes(32).toString("hex"),
      PAID_STRIPE_MODE: "test",
      PAID_STRIPE_SECRET_KEY: "synthetic-no-external-call",
      PAID_STRIPE_WEBHOOK_SECRET: "synthetic-webhook",
      PAID_STRIPE_ACCOUNT_ID: "acct_synthetic",
    },
  });
  try {
    const host = new URL(app.publicOrigin).host;
    const headers = { Host: host };
    // Node fetch deliberately rewrites Host. HTTP transport models the reverse proxy's
    // preserved public Host while connecting only to our loopback listener.
    const merchantHTTP = (
      path: string,
      options: {
        method?: string;
        headers?: Record<string, string>;
        body?: string;
      } = {},
    ) =>
      new Promise<{ status: number; value: any }>((resolve, reject) => {
        const req = httpRequest(
          app.origin + path,
          {
            method: options.method ?? "GET",
            headers: { ...headers, ...options.headers },
          },
          (res) => {
            const parts: Buffer[] = [];
            res.on("data", (chunk) => parts.push(Buffer.from(chunk)));
            res.on("end", () => {
              try {
                resolve({
                  status: res.statusCode!,
                  value: JSON.parse(Buffer.concat(parts).toString()),
                });
              } catch (error) {
                reject(error);
              }
            });
          },
        );
        req.on("error", reject);
        req.end(options.body);
      });
    const catalogResponse = await merchantHTTP("/api/catalog");
    assert.equal(catalogResponse.status, 200);
    const catalog = catalogResponse.value;
    assert.deepEqual(
      catalog.payment_methods.map((m: { id: string }) => m.id),
      ["stripe"],
    );
    assert.equal(catalog.payment_methods[0].mode, "test");
    assert.ok(
      catalog.offers.every(
        (o: { plan: { currency: string } }) => o.plan.currency === "TWD",
      ),
    );
    assert.equal(app.gateways.stripe!.fixture, undefined);
    assert.equal(app.host.environment, "test");
    const demo = await merchantHTTP("/api/session", {
      method: "POST",
      headers: {
        ...headers,
        Origin: app.publicOrigin,
        "Content-Type": "application/json",
      },
      body: JSON.stringify({ demo: true }),
    });
    assert.equal(demo.status, 403);
    assert.equal(demo.value.error, "demo_disabled");
    const notify = await merchantHTTP("/api/payments/stripe/notify", {
      method: "POST",
      headers: {
        ...headers,
        "Content-Type": "application/json",
        "stripe-signature": "t=1,v1=invalid",
      },
      body: JSON.stringify({ type: "checkout.session.completed" }),
    });
    assert.equal(notify.status, 401);
    const disabled = await merchantHTTP("/api/payments/oen/notify", {
      method: "POST",
      headers,
      body: "{}",
    });
    assert.equal(disabled.status, 404);
    const ledger = await app.store.pool.query(
      "SELECT count(*)::int AS count FROM sr_journal WHERE workspace=$1 AND environment=$2",
      [app.host.workspace, app.host.environment],
    );
    assert.equal(ledger.rows[0].count, 0);
  } finally {
    await app.close();
  }
});
