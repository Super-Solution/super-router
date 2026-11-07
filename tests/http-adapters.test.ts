import test from "node:test";
import assert from "node:assert/strict";
import { once } from "node:events";
import { createServer, type Server } from "node:http";
import type { AddressInfo } from "node:net";
import express from "express";
import Fastify from "fastify";
import { nodeListener } from "@supersolutions/node-fetch-adapters/node";
import { expressHandler } from "@supersolutions/node-fetch-adapters/express";
import { fastifyPlugin } from "@supersolutions/node-fetch-adapters/fastify";

let aborted = 0;
/** One handler, mounted everywhere: that is the contract under test. */
async function handler(request: Request): Promise<Response> {
  const url = new URL(request.url);
  if (url.pathname.endsWith("/echo")) {
    const text = await request.text();
    return Response.json(
      {
        method: request.method,
        path: url.pathname,
        query: url.search,
        type: request.headers.get("content-type"),
        text,
      },
      { status: 201, headers: { "x-custom": "1", "set-cookie": "a=1" } },
    );
  }
  if (url.pathname.endsWith("/stream")) {
    const encoder = new TextEncoder();
    let n = 0;
    const timer = {
      id: undefined as ReturnType<typeof setInterval> | undefined,
    };
    return new Response(
      new ReadableStream({
        start(controller) {
          request.signal.addEventListener("abort", () => {
            aborted++;
            clearInterval(timer.id);
            try {
              controller.close();
            } catch {
              /* already closed */
            }
          });
          controller.enqueue(encoder.encode("event: first\n\n"));
          timer.id = setInterval(() => {
            controller.enqueue(encoder.encode(`event: tick\ndata: ${++n}\n\n`));
            if (n === 3) {
              clearInterval(timer.id);
              controller.close();
            }
          }, 30);
        },
        cancel() {
          clearInterval(timer.id);
        },
      }),
      { headers: { "content-type": "text/event-stream" } },
    );
  }
  if (url.pathname.endsWith("/boom")) throw new Error("secret detail");
  return new Response(null, { status: 404 });
}

type Mount = {
  name: string;
  prefix: string;
  start: () => Promise<{ port: number; close: () => Promise<void> }>;
};
const listen = async (server: Server) => {
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  return {
    port: (server.address() as AddressInfo).port,
    close: () =>
      new Promise<void>((r) => {
        server.closeAllConnections();
        server.close(() => r());
      }),
  };
};
const mounts: Mount[] = [
  {
    name: "node:http",
    prefix: "",
    start: () => listen(createServer(nodeListener(handler))),
  },
  {
    name: "express (no body parser)",
    prefix: "",
    start: () => listen(createServer(express().use(expressHandler(handler)))),
  },
  {
    name: "express (express.json + express.text, mounted under /api)",
    prefix: "/api",
    start: () => {
      const app = express();
      app.use(
        "/api",
        express.json(),
        express.text({ type: "*/*" }),
        expressHandler(handler),
      );
      return listen(createServer(app));
    },
  },
  {
    name: "fastify (prefix /svc)",
    prefix: "/svc",
    start: async () => {
      const app = Fastify();
      await app.register(fastifyPlugin(handler), { prefix: "/svc" });
      await app.listen({ port: 0, host: "127.0.0.1" });
      return {
        port: (app.server.address() as AddressInfo).port,
        close: () => app.close(),
      };
    },
  },
];

for (const mount of mounts) {
  test(`fetch adapter: ${mount.name}`, async () => {
    const { port, close } = await mount.start();
    const base = `http://127.0.0.1:${port}${mount.prefix}`;
    try {
      const json = '{"hello":"world"}';
      const echo = await fetch(base + "/echo?a=1", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: json,
      });
      assert.equal(echo.status, 201);
      assert.equal(echo.headers.get("x-custom"), "1");
      assert.match(echo.headers.get("set-cookie") ?? "", /a=1/);
      const body = (await echo.json()) as Record<string, string>;
      assert.equal(body.method, "POST");
      assert.equal(body.path, mount.prefix + "/echo");
      assert.equal(body.query, "?a=1");
      assert.deepEqual(JSON.parse(body.text!), { hello: "world" });

      assert.equal((await fetch(base + "/echo")).status, 201);
      assert.equal((await fetch(base + "/nothing")).status, 404);

      // Unhandled handler errors become a generic 500 with no detail.
      const boom = await fetch(base + "/boom");
      assert.equal(boom.status, 500);
      assert.deepEqual(await boom.json(), {
        error: { code: "internal_error" },
      });

      // Streaming: the first chunk arrives before the stream ends.
      const stream = await fetch(base + "/stream");
      assert.equal(stream.headers.get("content-type"), "text/event-stream");
      const reader = stream.body!.getReader();
      const first = await reader.read();
      assert.match(new TextDecoder().decode(first.value), /event: first/);
      let rest = "";
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        rest += new TextDecoder().decode(value);
      }
      assert.match(rest, /data: 3/);

      // A client that disconnects aborts request.signal.
      const before = aborted;
      const controller = new AbortController();
      const live = await fetch(base + "/stream", { signal: controller.signal });
      await live.body!.getReader().read();
      controller.abort();
      for (let i = 0; i < 50 && aborted === before; i++)
        await new Promise((r) => setTimeout(r, 20));
      assert.equal(aborted, before + 1);
    } finally {
      await close();
    }
  });
}
