import test from "node:test";
import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { once } from "node:events";
import { Router, MockProvider } from "@superrouter/core";
import { SuperRouterClient } from "@superrouter/sdk";
import {
  PostgresStore,
  LOCAL_DATABASE_URL,
} from "@superrouter/storage-postgres";
import { createRouterServer } from "../apps/server/server.js";
import {
  principal,
  digestKey,
  seed,
  demoInput,
  demoService,
} from "../scripts/demo-config.js";
test("HTTP + SDK: real SSE, replay, unauthorized and customer spoofing rejected", async () => {
  const store = new PostgresStore(
    process.env.DATABASE_URL ?? LOCAL_DATABASE_URL,
  );
  await store.migrate();
  const p = { ...principal, workspace: "http-" + randomUUID() };
  await seed(store, p);
  const provider = new MockProvider();
  const key = randomUUID();
  const server = createRouterServer(
    new Router(store, { mock: provider }, digestKey),
    store,
    [{ key, principal: p }],
  );
  try {
    server.listen(0, "127.0.0.1");
    await once(server, "listening");
    const addr = server.address() as { port: number };
    const url = "http://127.0.0.1:" + addr.port;
    const client = new SuperRouterClient(url, key);
    const events = await Array.fromAsync(client.stream(demoInput, "stream"));
    assert.deepEqual(
      events.map((x) => x.type),
      ["request.started", "output.delta", "request.completed"],
    );
    assert.equal(events.at(-1)?.billing_status, "settled");
    const id = events[0]!.request_id;
    assert.equal(
      ((await client.getRequest(id)) as any).execution_status,
      "succeeded",
    );
    const replay = await Array.fromAsync(client.stream(demoInput, "stream"));
    assert.equal(replay[0]?.type, "request.replayed");
    assert.equal(provider.calls, 1);
    assert.equal((await fetch(url + "/v1/services")).status, 401);
    await assert.rejects(
      () => client.infer({ ...demoInput, customer_id: "victim" }, "spoof"),
      /scope_denied/,
    );
    const bad = await fetch(url + "/v1/inferences", {
      method: "POST",
      headers: { Authorization: "Bearer " + key, "Idempotency-Key": "bad" },
      body: JSON.stringify({ ...demoInput, tools: [] }),
    });
    assert.equal(bad.status, 400);
    assert.equal(provider.calls, 1);
  } finally {
    await new Promise<void>((r) => server.close(() => r()));
    await store.close();
  }
});
test("cancel intent during running request results in unknown hold, not automatic free retry", async () => {
  const store = new PostgresStore(
    process.env.DATABASE_URL ?? LOCAL_DATABASE_URL,
  );
  await store.migrate();
  const p = { ...principal, workspace: "cancel-" + randomUUID() };
  await seed(store, p);
  const mock = new MockProvider("success", 500);
  const router = new Router(store, { mock }, digestKey);
  try {
    const result = await router.infer(p, demoInput, "cancel", async (e) => {
      if (e.type === "request.started")
        setTimeout(() => {
          void store.cancel(p, e.request_id);
        }, 20);
    });
    assert.equal(result.billing_status, "pending");
    assert.equal(mock.calls, 1);
    assert.equal((await store.balance(p))[0].held, "2144");
  } finally {
    await store.close();
  }
});

test("Cloud HTTP rejects static principals while self-host keeps static-key compatibility", async () => {
  const store = new PostgresStore(
    process.env.DATABASE_URL ?? LOCAL_DATABASE_URL,
  );
  const router = new Router(store, {}, digestKey);
  const credentials = [{ key: randomUUID(), principal }];
  try {
    assert.throws(
      () => createRouterServer(router, store, credentials, "cloud"),
      /cloud_requires_control_plane_authentication/,
    );
    // Rejection happens during construction, before a socket can expose privileged static principals.
    const server = createRouterServer(router, store, credentials, "self-host");
    server.listen(0, "127.0.0.1");
    await once(server, "listening");
    try {
      const address = server.address();
      assert.ok(address && typeof address === "object");
      const response = await fetch(
        `http://127.0.0.1:${address.port}/v1/services`,
      );
      assert.equal(response.status, 401);
    } finally {
      await new Promise<void>((resolve) => server.close(() => resolve()));
    }
  } finally {
    await store.close();
  }
});
