import test from "node:test";
import assert from "node:assert/strict";
import { randomUUID, createHmac } from "node:crypto";
import { startPaidChat } from "../examples/paid-chat/server.js";
import {
  catalog,
  services,
  chatService,
} from "../examples/paid-chat/config.js";
import {
  formatIntegerUnits,
  currencyScale,
} from "@supersolutions/super-router-ui";
import { validatePreset } from "@supersolutions/super-router-billing-presets";
test("paid preset exact formatting and validation", () => {
  assert.equal(formatIntegerUnits("30000000001"), "30.000000001");
  assert.equal(
    formatIntegerUnits("999999999999999999999999999999"),
    "999999999999999999999.999999999",
  );
  assert.equal(formatIntegerUnits("24900", 2), "249");
  assert.equal(currencyScale("JPY"), 0);
  assert.throws(() => currencyScale("XYZ"));
  assert.throws(() => validatePreset([catalog[0]!, catalog[0]!]));
});
test("customer sandbox payment → credits → chat/media → cancellation; isolation and replay", async () => {
  const app = await startPaidChat({
    port: 4319,
    checkoutPort: 4320,
    workspace: "paid-test-" + randomUUID(),
    quiet: true,
    mockDelay: 200,
  });
  let cookie = "";
  const req = async (
    path: string,
    body?: unknown,
    options: {
      cookie?: string;
      origin?: string;
      key?: string;
      method?: string;
    } = {},
  ) => {
    if (
      path === "/api/infer" &&
      body &&
      typeof body === "object" &&
      !("expected_service_version" in body)
    )
      body = { ...body, expected_service_version: 1 };
    const res = await fetch(app.origin + path, {
      method: options.method ?? (body === undefined ? "GET" : "POST"),
      headers: {
        Cookie: options.cookie ?? cookie,
        ...(body === undefined
          ? {}
          : {
              "Content-Type": "application/json",
              Origin: options.origin ?? app.origin,
              "Idempotency-Key": options.key ?? randomUUID(),
            }),
      },
      ...(body === undefined ? {} : { body: JSON.stringify(body) }),
    });
    const value = await res.json();
    return { res, value };
  };
  const issue = async (customer: string) => {
    await app.control.customer(
      app.host,
      { id: customer, external_id: customer },
      "customer:" + customer,
    );
    return app.control.issueKey(
      app.host,
      {
        member_id: "paid-app",
        customer_id: customer,
        scopes: ["invoke", "credits:read"],
        service_ids: Object.values(services).map((s) => s.id),
        expires_at: new Date(Date.now() + 3600000).toISOString(),
      },
      "key:" + customer,
    );
  };
  try {
    assert.equal((await req("/api/billing/me")).res.status, 401);
    assert.equal(
      (
        await req(
          "/api/session",
          { demo: true },
          { origin: "http://evil.example" },
        )
      ).res.status,
      403,
    );
    assert.equal(
      (await req("/api/session", { demo: true, customer: "other" })).res.status,
      400,
    );
    let result = await req("/api/session", { demo: true });
    assert.equal(result.res.status, 200);
    cookie = result.res.headers.get("set-cookie")!.split(";")[0]!;
    assert.match(result.res.headers.get("set-cookie")!, /HttpOnly/);
    assert.match(result.res.headers.get("set-cookie")!, /SameSite=Strict/);
    assert.ok(
      ["subscription_required", "insufficient_balance"].includes(
        (await req("/api/infer", { mode: "chat", prompt: "before payment" }))
          .value.error,
      ),
    );
    assert.equal(
      (
        await req("/api/billing/checkout", {
          offer_id: "starter-v1",
          amount_minor: "1",
        })
      ).res.status,
      400,
    );
    const checkoutKey = randomUUID();
    result = await req(
      "/api/billing/checkout",
      { offer_id: "starter-v1" },
      { key: checkoutKey },
    );
    assert.equal(result.res.status, 200);
    const checkout = result.value;
    const same = await req(
      "/api/billing/checkout",
      { offer_id: "starter-v1" },
      { key: checkoutKey },
    );
    assert.deepEqual(same.value, checkout);
    const freshKey = await req("/api/billing/checkout", {
      offer_id: "starter-v1",
    });
    assert.equal(
      freshKey.value.order_id,
      checkout.order_id,
      "new idempotency key does not create second subscription/order",
    );
    assert.equal(
      (
        await req(
          "/api/billing/checkout",
          { offer_id: "creator-v1" },
          { key: checkoutKey },
        )
      ).res.status,
      409,
    );
    assert.equal((await req("/api/billing/me")).value.buckets.length, 0);
    const returned = await fetch(app.origin + "/?order=" + checkout.order_id, {
      headers: { Cookie: cookie },
    });
    assert.equal(returned.status, 200);
    assert.equal(
      (await req("/api/billing/orders/" + checkout.order_id)).value
        .fulfillment_status,
      "pending",
      "return URL does not grant",
    );
    const page = await fetch(checkout.redirect_url, {
      headers: { Cookie: cookie },
    });
    assert.equal(page.status, 200);
    assert.match(await page.text(), /無真實扣款/);
    const ticket = new URL(checkout.redirect_url).searchParams.get("ticket")!;
    const pay = async (t = ticket, c = cookie, origin = app.checkoutOrigin) =>
      fetch(app.checkoutOrigin + "/complete", {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          Origin: origin,
          Cookie: c,
        },
        body: JSON.stringify({ ticket: t }),
      });
    const projected = JSON.parse(
      Buffer.from(ticket.split(".")[0]!, "base64url").toString(),
    );
    assert.deepEqual(Object.keys(projected).sort(), [
      "amount_minor",
      "currency",
      "id",
    ]);
    const signFixture = (payload: unknown) => {
      const data = Buffer.from(JSON.stringify(payload)).toString("base64url");
      return (
        data +
        "." +
        createHmac(
          "sha256",
          process.env.PAID_SANDBOX_SECRET ??
            "local-only-paid-chat-sandbox-fixture-not-for-production",
        )
          .update(data)
          .digest("hex")
      );
    };
    const readCheckout = (value: string) =>
      fetch(
        app.checkoutOrigin + "/checkout?ticket=" + encodeURIComponent(value),
        { headers: { Cookie: cookie }, redirect: "manual" },
      );
    const injection = "</script><script>globalThis.injected=true</script>";
    for (const invalid of [
      ticket + "." + injection,
      ticket + ".",
      ticket + "..",
      signFixture({ ...projected, amount_minor: "1" }),
      signFixture({ ...projected, currency: "USD" }),
      signFixture({ ...projected, currency: injection }),
      signFixture({ ...projected, id: injection }),
    ]) {
      const get = await readCheckout(invalid);
      assert.equal(get.status, 403);
      const text = await get.text();
      assert.equal(text, JSON.stringify({ error: "invalid_ticket" }));
      assert.ok(!text.includes(injection));
      const post = await pay(invalid);
      assert.equal(post.status, 403);
      assert.deepEqual(await post.json(), { error: "invalid_ticket" });
      assert.equal(
        await app.adapter.lookup(checkout.order_id),
        null,
        "invalid forged snapshot does not record payment",
      );
    }
    const legacyRow = (
      await app.store.pool.query(
        "SELECT * FROM sr_payment_orders WHERE workspace=$1 AND environment=$2 AND id=$3",
        [app.host.workspace, app.host.environment, checkout.order_id],
      )
    ).rows[0];
    const legacyTicket = signFixture({
      ...legacyRow,
      legacy_ignored: injection,
    });
    assert.deepEqual(
      app.adapter.ticket(legacyTicket),
      projected,
      "signed legacy full-row payload is validated and projected",
    );
    const legacyPage = await readCheckout(legacyTicket);
    assert.equal(legacyPage.status, 200);
    const legacyHTML = await legacyPage.text();
    assert.match(legacyHTML, /TWD 99.00/);
    assert.match(legacyHTML, /<script src="\/checkout.js"><\/script>/);
    assert.ok(!legacyHTML.includes(legacyTicket));
    assert.ok(!legacyHTML.includes(injection));
    assert.ok(!legacyHTML.includes("<script>"));
    const staticScript = await fetch(app.checkoutOrigin + "/checkout.js");
    assert.equal(staticScript.status, 200);
    assert.ok(
      (await staticScript.text()).includes(
        "new URL(location.href).searchParams.get('ticket')",
      ),
    );
    assert.equal((await pay(ticket + "x")).status, 403);
    assert.equal((await pay(ticket, cookie, app.origin)).status, 403);
    const second = await issue("second");
    result = await req("/api/session", { key: second.token });
    const secondCookie = result.res.headers.get("set-cookie")!.split(";")[0]!;
    // Session rotation invalidates the previous cookie. Reconnect demo to obtain its fresh session.
    result = await req("/api/session", { demo: true }, { cookie: "" });
    cookie = result.res.headers.get("set-cookie")!.split(";")[0]!;
    assert.equal(
      (
        await req("/api/billing/orders/" + checkout.order_id, undefined, {
          cookie: secondCookie,
        })
      ).res.status,
      404,
    );
    const sub = (await req("/api/billing/me")).value.subscriptions[0];
    assert.equal(
      (
        await req(
          "/api/billing/subscription/cancel",
          { subscription_id: sub.id },
          { cookie: secondCookie },
        )
      ).res.status,
      404,
    );
    assert.equal((await pay(ticket, secondCookie)).status, 404);
    assert.equal((await pay(legacyTicket)).status, 200);
    assert.equal((await pay()).status, 200);
    result = await req("/api/billing/me");
    assert.equal(result.value.buckets.length, 1);
    assert.equal(result.value.buckets[0].available, "10000000000");
    assert.equal(
      (await req("/api/billing/orders/" + checkout.order_id)).value
        .fulfillment_status,
      "fulfilled",
    );
    const inferKey = randomUUID();
    result = await req(
      "/api/infer",
      { mode: "chat", prompt: "hello" },
      { key: inferKey },
    );
    assert.equal(result.res.status, 200);
    assert.equal(result.value.billing_status, "settled");
    assert.ok(result.value.output.content);
    const charge = BigInt(result.value.billing.charges[0].atoms);
    assert.ok(charge > 0n);
    const replay = await req(
      "/api/infer",
      { mode: "chat", prompt: "hello" },
      { key: inferKey },
    );
    assert.equal(replay.value.replayed, true);
    for (const [mode, quantity, atoms, meter, units] of [
      ["image", 2, "400000000", "images", 2],
      ["video", 5, "250000000", "video_ms", 5000],
      ["audio", 4, "40000000", "audio_ms", 4000],
      ["task", 1, "100000000", "tasks", 1],
    ] as const) {
      result = await req("/api/infer", {
        mode,
        prompt: "sandbox creation",
        quantity,
      });
      assert.equal(result.res.status, 200, JSON.stringify(result.value));
      assert.equal(result.value.billing_status, "settled");
      assert.equal(result.value.billing.charges[0].atoms, atoms);
      assert.equal(result.value.usage.meters[meter], units);
    }
    const snapshot = (await req("/api/billing/me")).value;
    assert.equal(snapshot.usage.length, 5);
    assert.equal(
      snapshot.buckets[0].available,
      (10000000000n - charge - 790000000n).toString(),
    );
    const serialized = JSON.stringify(snapshot);
    for (const forbidden of [
      "provider_spent",
      "offerings",
      "hello",
      "hash",
      "token",
    ])
      assert.ok(!serialized.includes(forbidden));
    assert.equal(
      (await req("/api/billing/me", undefined, { cookie: secondCookie })).value
        .usage.length,
      0,
    );
    const abortKey = randomUUID(),
      controller = new AbortController(),
      callsBefore = app.mock.calls;
    const canceled = fetch(app.origin + "/api/infer", {
      method: "POST",
      headers: {
        Cookie: cookie,
        Origin: app.origin,
        "Content-Type": "application/json",
        "Idempotency-Key": abortKey,
      },
      body: JSON.stringify({
        mode: "chat",
        prompt: "stop after dispatch",
        expected_service_version: 1,
      }),
      signal: controller.signal,
    }).catch((error) => error);
    for (let i = 0; i < 50 && app.mock.calls === callsBefore; i++)
      await new Promise((r) => setTimeout(r, 10));
    assert.equal(app.mock.calls, callsBefore + 1);
    controller.abort();
    assert.equal((await canceled).name, "AbortError");
    let stopped: any;
    for (let i = 0; i < 50; i++) {
      stopped = (await req("/api/billing/me")).value;
      if (stopped.usage.some((u: any) => u.billing === "pending")) break;
      await new Promise((r) => setTimeout(r, 10));
    }
    assert.ok(stopped.usage.some((u: any) => u.billing === "pending"));
    assert.ok(
      BigInt(stopped.buckets[0].held) > 0n,
      "unknown dispatch retains a hold instead of refunding",
    );
    const retryStop = await req(
      "/api/infer",
      { mode: "chat", prompt: "stop after dispatch" },
      { key: abortKey },
    );
    assert.equal(retryStop.value.replayed, true);
    assert.equal(app.mock.calls, callsBefore + 1);

    result = await req("/api/billing/subscription/cancel", {
      subscription_id: sub.id,
    });
    assert.equal(result.res.status, 200);
    assert.equal(
      (await req("/api/billing/me")).value.subscriptions[0].status,
      "active",
    );
    // Direct public customer use case must revalidate even on a stored command replay.
    const secondPrincipal = await app.control.authenticate(second.token!);
    const secondCheckout = await app.control.customerCheckout(
      app.host,
      secondPrincipal,
      { plan_id: "starter", plan_version: 1 },
      app.integration,
      "second-order",
    );
    const cancelKey = "second-cancel";
    await app.control.customerCancel(
      app.host,
      secondPrincipal,
      secondCheckout.subscription_id,
      cancelKey,
    );
    await app.control.revokeKey(app.host, second.id, "revoke-second");
    await assert.rejects(
      () =>
        app.control.customerCheckout(
          app.host,
          secondPrincipal,
          { plan_id: "starter", plan_version: 1 },
          app.integration,
          "second-order",
        ),
      /invalid_key/,
    );
    await assert.rejects(
      () =>
        app.control.customerCancel(
          app.host,
          secondPrincipal,
          secondCheckout.subscription_id,
          cancelKey,
        ),
      /invalid_key/,
    );
    assert.equal(
      (await req("/api/billing/me", undefined, { cookie: secondCookie })).res
        .status,
      401,
    );
    const logout = await req("/api/logout", {}, { cookie: secondCookie });
    assert.equal(logout.res.status, 200);
    assert.match(logout.res.headers.get("set-cookie")!, /Max-Age=0/);
    const foreign = await app.control.bootstrap(
      "foreign-" + randomUUID(),
      "test",
    );
    assert.equal(
      (await req("/api/session", { key: foreign.token })).res.status,
      403,
    );
    const foreignPrincipal = await app.control.authenticate(foreign.token);
    await app.store.publish(foreignPrincipal, chatService);
    await app.control.member(
      foreignPrincipal,
      { id: "foreign-app", role: "app" },
      "member",
    );
    await app.control.customer(
      foreignPrincipal,
      { id: "demo-customer", external_id: "foreign-demo" },
      "customer",
    );
    const foreignApp = await app.control.issueKey(
      foreignPrincipal,
      {
        member_id: "foreign-app",
        customer_id: "demo-customer",
        scopes: ["invoke", "credits:read"],
        service_ids: [chatService.id],
        expires_at: new Date(Date.now() + 3600000).toISOString(),
      },
      "app-key",
    );
    assert.equal(
      (await req("/api/session", { key: foreignApp.token })).res.status,
      403,
    );
    await app.store.publish(app.host, {
      ...chatService,
      version: 2,
      price: {
        ...chatService.price,
        input: "4000",
        output: "6000",
      },
    });
    const changedCatalog = (await req("/api/catalog")).value;
    assert.equal(changedCatalog.services.chat.version, 2);
    assert.match(
      changedCatalog.services.chat.rate,
      /輸入 1 credit＝250000 tokens/,
    );
    assert.match(
      changedCatalog.services.chat.rate,
      /輸出 0.000006 credits／1 tokens/,
    );
    assert.ok(!JSON.stringify(changedCatalog).includes("offerings"));
    const beforeStale = (await req("/api/billing/me")).value,
      callsBeforeStale = app.mock.calls;
    const stale = await req("/api/infer", {
      mode: "chat",
      prompt: "quoted at v1",
      expected_service_version: 1,
    });
    assert.equal(stale.res.status, 409);
    assert.equal(stale.value.error, "service_version_changed");
    assert.equal(app.mock.calls, callsBeforeStale);
    const afterStale = (await req("/api/billing/me")).value;
    assert.equal(afterStale.usage.length, beforeStale.usage.length);
    assert.equal(
      afterStale.buckets[0].available,
      beforeStale.buckets[0].available,
    );
    const acceptedReplay = await req(
      "/api/infer",
      { mode: "chat", prompt: "hello", expected_service_version: 1 },
      { key: inferKey },
    );
    assert.equal(acceptedReplay.value.replayed, true);
    const freshQuote = await req("/api/infer", {
      mode: "chat",
      prompt: "reviewed v2",
      expected_service_version: changedCatalog.services.chat.version,
    });
    assert.equal(freshQuote.res.status, 200);
    assert.equal(freshQuote.value.service_version, 2);
    assert.equal(freshQuote.value.billing.charges[0].atoms, "70000");
    const missingVersion = await req("/api/infer", {
      mode: "chat",
      prompt: "no quote",
      expected_service_version: undefined,
    });
    assert.equal(missingVersion.res.status, 400);
    const ownerKey = "sr_" + "0".repeat(64);
    assert.equal(
      (await req("/api/session", { key: ownerKey })).res.status,
      401,
    );
  } finally {
    await app.close();
  }
});

test("expired opaque session cannot read billing and can still logout", async () => {
  const app = await startPaidChat({
    port: 4319,
    checkoutPort: 4320,
    workspace: "paid-expired-" + randomUUID(),
    quiet: true,
    sessionMs: 20,
  });
  try {
    const login = await fetch(app.origin + "/api/session", {
      method: "POST",
      headers: { "Content-Type": "application/json", Origin: app.origin },
      body: JSON.stringify({ demo: true }),
    });
    assert.equal(login.status, 200);
    const cookie = login.headers.get("set-cookie")!.split(";")[0]!;
    await new Promise((r) => setTimeout(r, 40));
    const billing = await fetch(app.origin + "/api/billing/me", {
      headers: { Cookie: cookie },
    });
    assert.equal(billing.status, 401);
    const logout = await fetch(app.origin + "/api/logout", {
      method: "POST",
      headers: {
        Cookie: cookie,
        Origin: app.origin,
        "Content-Type": "application/json",
      },
      body: "{}",
    });
    assert.equal(logout.status, 200);
  } finally {
    await app.close();
  }
});
