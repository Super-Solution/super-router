// The router API is one fetch handler; prove it mounts unchanged on node:http, Express and Fastify,
// over the in-memory store (no database needed).
import test from "node:test";
import assert from "node:assert/strict";
import { once } from "node:events";
import { createServer } from "node:http";
import type { AddressInfo } from "node:net";
import express from "express";
import Fastify from "fastify";
import { MockProvider, Router } from "@supersolutions/super-router-core";
import { MemoryStore } from "@supersolutions/super-router-storage-memory";
import { createRouterHandler } from "@supersolutions/super-router-server";
import { expressHandler } from "@supersolutions/super-router-fetch-adapters/express";
import { fastifyPlugin } from "@supersolutions/super-router-fetch-adapters/fastify";
import { nodeListener } from "@supersolutions/super-router-fetch-adapters/node";
import { parseSSE } from "@supersolutions/super-router-contracts";
import {
  principal,
  digestKey,
  seed,
  demoInput,
  demoService,
} from "../scripts/demo-config.js";

async function fixture() {
  const store = new MemoryStore();
  await seed(store as never, principal, demoService);
  const provider = new MockProvider();
  const key = "k-" + Math.random();
  const handler = createRouterHandler({
    router: new Router(store, { mock: provider }, digestKey),
    store,
    keys: [{ key, principal }],
  });
  return { handler, key, provider };
}
const mounts = {
  "node:http": async (h: (r: Request) => Promise<Response>) => {
    const server = createServer(nodeListener(h));
    server.listen(0, "127.0.0.1");
    await once(server, "listening");
    return {
      base: "http://127.0.0.1:" + (server.address() as AddressInfo).port,
      close: async () => void server.close(),
    };
  },
  express: async (h: (r: Request) => Promise<Response>) => {
    const app = express();
    app.use("/router", express.json(), expressHandler(h));
    const server = createServer(app);
    server.listen(0, "127.0.0.1");
    await once(server, "listening");
    return {
      base:
        "http://127.0.0.1:" +
        (server.address() as AddressInfo).port +
        "/router",
      close: async () => void server.close(),
    };
  },
  fastify: async (h: (r: Request) => Promise<Response>) => {
    const app = Fastify();
    await app.register(fastifyPlugin(h), { prefix: "/router" });
    await app.listen({ port: 0, host: "127.0.0.1" });
    return {
      base:
        "http://127.0.0.1:" +
        (app.server.address() as AddressInfo).port +
        "/router",
      close: () => app.close(),
    };
  },
};

for (const [name, mount] of Object.entries(mounts))
  test(`router API on ${name} over the memory store`, async () => {
    const { handler, key, provider } = await fixture();
    // The handler is mounted under a prefix in some frameworks: strip it so the API paths stay /v1/...
    const prefix = name === "node:http" ? "" : "/router";
    const strip = async (r: Request) =>
      handler(prefix ? new Request(r.url.replace(prefix, ""), r) : r);
    const { base, close } = await mount(strip);
    const auth = { Authorization: "Bearer " + key };
    try {
      assert.equal((await fetch(base + "/v1/services")).status, 401);
      const services = (await (
        await fetch(base + "/v1/services", { headers: auth })
      ).json()) as { data: { id: string }[] };
      assert.deepEqual(
        services.data.map((s) => s.id),
        ["chat"],
      );

      const done = await fetch(base + "/v1/inferences", {
        method: "POST",
        headers: {
          ...auth,
          "Idempotency-Key": "one",
          "content-type": "application/json",
        },
        body: JSON.stringify(demoInput),
      });
      assert.equal(done.status, 200);
      const output = (await done.json()) as {
        billing_status: string;
        execution_status: string;
      };
      assert.deepEqual(
        [output.execution_status, output.billing_status],
        ["succeeded", "settled"],
      );

      const streamed = await fetch(base + "/v1/inferences", {
        method: "POST",
        headers: {
          ...auth,
          "Idempotency-Key": "two",
          "content-type": "application/json",
        },
        body: JSON.stringify({ ...demoInput, stream: true }),
      });
      assert.equal(streamed.headers.get("content-type"), "text/event-stream");
      const events = [];
      for await (const e of parseSSE(streamed.body!)) events.push(e.event);
      assert.deepEqual(events, [
        "request.started",
        "output.delta",
        "request.completed",
      ]);

      const bad = await fetch(base + "/v1/inferences", {
        method: "POST",
        headers: {
          ...auth,
          "Idempotency-Key": "three",
          "content-type": "application/json",
        },
        body: JSON.stringify({ ...demoInput, customer_id: "victim" }),
      });
      assert.equal(bad.status, 403);
      assert.equal(
        ((await bad.json()) as { error: { code: string } }).error.code,
        "scope_denied",
      );
      assert.equal(provider.calls, 2);
    } finally {
      await close();
    }
  });

test("a chunked body without Content-Length is cut off at the limit instead of buffered", async () => {
  const { handler, key } = await fixture();
  let pulled = 0;
  const chunk = new Uint8Array(64 * 1024).fill(0x20);
  const body = new ReadableStream<Uint8Array>({
    pull(controller) {
      pulled += chunk.length;
      controller.enqueue(chunk); // endless: a client that never stops sending
    },
  });
  const request = new Request("http://localhost/v1/inferences", {
    method: "POST",
    headers: { authorization: "Bearer " + key, "idempotency-key": "big" },
    body,
    duplex: "half",
  } as RequestInit);
  const response = await handler(request);
  assert.equal(response.status, 413);
  assert.equal(((await response.json()) as any).error.code, "body_too_large");
  assert.ok(
    pulled <= 256_000 + 3 * 64 * 1024,
    `read ${pulled} bytes before refusing`,
  );
});

test("an empty configured key does not authenticate a request without credentials", async () => {
  const store = new MemoryStore();
  await seed(store as never, principal, demoService);
  const handler = createRouterHandler({
    router: new Router(store, { mock: new MockProvider() }, digestKey),
    store,
    keys: [{ key: "", principal }],
  });
  const res = await handler(new Request("http://x/v1/services"));
  assert.equal(res.status, 401);
});

test("an offering whose provider is named like an Object.prototype member is an unavailable provider", async () => {
  const store = new MemoryStore();
  const service = JSON.parse(JSON.stringify(demoService));
  for (const offering of service.offerings) offering.provider = "constructor";
  await seed(store as never, principal, service);
  const router = new Router(store, { mock: new MockProvider() }, digestKey);
  await assert.rejects(
    () => router.infer(principal, demoInput, "proto-key"),
    (e: unknown) => (e as { code?: string }).code === "provider_unavailable",
  );
});
