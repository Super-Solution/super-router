import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { createHash, randomUUID } from "node:crypto";
import { fileURLToPath } from "node:url";
import {
  ControlPlane,
  PostgresStore,
  LOCAL_DATABASE_URL,
} from "@supersolutions/super-router-storage-postgres";
import {
  Router,
  MockProvider,
  MockTaskProvider,
} from "@supersolutions/super-router-core";
import { RouterError } from "@supersolutions/super-router-contracts";
import {
  createCustomerBillingBFF,
  publishPreset,
  readJSON,
  sendJSON,
} from "@supersolutions/super-router-billing-presets";
import { buildGateways, type GatewayId } from "./gateways.js";
import { LocalSandboxPayments } from "./sandbox.js";
import { catalog, services, chatService, taskServices } from "./config.js";
export async function startPaidChat(
  options: {
    port?: number;
    checkoutPort?: number;
    databaseURL?: string;
    workspace?: string;
    quiet?: boolean;
    sessionMs?: number;
    mockDelay?: number;
    paymentMode?: "fixture" | "merchant";
    gatewayEnv?: NodeJS.ProcessEnv;
    publicOrigin?: string;
  } = {},
) {
  const port = options.port ?? Number(process.env.PAID_CHAT_PORT ?? 4317),
    checkoutPort =
      options.checkoutPort ?? Number(process.env.PAID_CHECKOUT_PORT ?? 4318);
  if (
    ![port, checkoutPort].every(
      (p) => Number.isInteger(p) && p >= 1024 && p <= 65535,
    ) ||
    port === checkoutPort
  )
    throw new Error("Distinct valid local ports required");
  const origin = `http://127.0.0.1:${port}`,
    checkoutOrigin = `http://127.0.0.1:${checkoutPort}`;
  const gatewayEnv = options.gatewayEnv ?? process.env;
  const paymentMode =
    options.paymentMode ?? gatewayEnv.PAID_PAYMENT_MODE ?? "fixture";
  if (paymentMode !== "fixture" && paymentMode !== "merchant")
    throw new Error("Invalid PAID_PAYMENT_MODE");
  const secret =
    paymentMode === "fixture"
      ? (gatewayEnv.PAID_SANDBOX_SECRET ??
        "local-only-paid-chat-sandbox-fixture-not-for-production")
      : gatewayEnv.PAID_SERVER_SECRET;
  if (!secret || secret.length < 32)
    throw new Error(
      "Merchant mode requires server-only PAID_SERVER_SECRET >=32 characters",
    );
  const workspace =
    options.workspace ??
    gatewayEnv.PAID_WORKSPACE ??
    (paymentMode === "fixture" ? "paid-chat-local-v1" : "");
  if (!workspace) throw new Error("Merchant mode requires PAID_WORKSPACE");
  const hostToken =
    paymentMode === "fixture"
      ? "sr_" +
        createHash("sha256")
          .update(secret + ":" + workspace + ":host")
          .digest("hex")
      : gatewayEnv.PAID_HOST_KEY;
  if (!hostToken || !/^sr_[a-f0-9]{64}$/.test(hostToken))
    throw new Error("Merchant mode requires server-only PAID_HOST_KEY");
  const gateways = buildGateways({
    mode: paymentMode,
    checkoutOrigin,
    workspace,
    secret,
    publicOrigin: options.publicOrigin ?? gatewayEnv.PAID_PUBLIC_ORIGIN,
    env: gatewayEnv,
  });
  const environment = Object.values(gateways).every((g) => g.mode === "live")
    ? "live"
    : "test";
  const applicationOrigin =
    paymentMode === "merchant"
      ? new URL(options.publicOrigin ?? gatewayEnv.PAID_PUBLIC_ORIGIN!).origin
      : origin;
  const primary = gateways.sandbox ?? Object.values(gateways)[0]!;
  const integration = primary.integration,
    adapter = primary.adapter as LocalSandboxPayments;
  const offers = [
    ...catalog.map((o) => ({
      ...o,
      payment_methods: ["sandbox", "stripe", "oen"],
    })),
    ...catalog.map((o) => ({
      ...o,
      id: o.plan.id + "-cny-v1",
      payment_methods: ["epay"],
      plan: {
        ...o.plan,
        id: o.plan.id + "-cny",
        currency: "CNY",
        amount_minor: o.plan.id === "starter" ? "1900" : "4900",
      },
    })),
  ].filter((o) => o.payment_methods.some((id) => gateways[id]));
  const store = new PostgresStore(
    options.databaseURL ?? process.env.DATABASE_URL ?? LOCAL_DATABASE_URL,
  );
  await store.migrate();
  const control = new ControlPlane(
    store,
    Object.values(gateways).map((g) => ({
      id: g.integration,
      workspace,
      environment,
      mode: "hosted" as const,
      adapter: g.adapter,
    })),
  );
  try {
    await control.bootstrap(workspace, environment, "owner", hostToken);
  } catch (e) {
    if (!(e instanceof RouterError) || e.code !== "already_bootstrapped")
      throw e;
  }
  const host = await control.authenticate(hostToken);
  if (host.workspace !== workspace || host.environment !== environment)
    throw new Error("Host key belongs to a different workspace/environment");
  for (const s of [chatService, ...taskServices]) {
    try {
      await store.getService(host, s.id, s.version);
    } catch {
      await store.publish(host, s);
    }
  }
  if (paymentMode === "fixture") {
    await control.customer(
      host,
      { id: "demo-customer", external_id: "local-demo" },
      "paid-demo-customer",
    );
    await control.member(
      host,
      { id: "paid-app", role: "app", active: true },
      "paid-app-member",
    );
  }
  await publishPreset(control, host, offers);
  let demoKey: string | undefined;
  const mock = new MockProvider("success", options.mockDelay);
  const router = new Router(
    store,
    {
      mock,
      "mock-task": new MockTaskProvider(),
    },
    secret,
  );
  const bff = createCustomerBillingBFF({
    origin: applicationOrigin,
    control,
    host,
    router,
    offers,
    paymentMethods: Object.values(gateways).map((g) => ({
      id: g.id,
      name: g.name,
      mode: g.mode,
      integration: g.integration,
      currencies: g.currencies,
      allowedOrigins: g.allowedOrigins,
      capabilities: g.adapter.capabilities,
    })),
    integration,
    allowedCheckoutOrigins: [checkoutOrigin],
    services,
    sandbox: paymentMode === "fixture",
    sessionMs: options.sessionMs,
    ...(paymentMode === "fixture"
      ? {
          demoKey: async (fresh = false) => {
            if (!fresh && demoKey) return demoKey;
            const customer = fresh ? "demo-" + randomUUID() : "demo-customer";
            if (fresh)
              await control.customer(
                host,
                { id: customer, external_id: customer },
                "local-new:" + customer,
              );
            const k = await control.issueKey(
              host,
              {
                member_id: "paid-app",
                customer_id: customer,
                scopes: ["invoke", "credits:read"],
                service_ids: Object.values(services).map((s) => s.id),
                expires_at: new Date(Date.now() + 86400000).toISOString(),
              },
              "local-session-" + randomUUID(),
            );
            if (!fresh) demoKey = k.token!;
            return k.token!;
          },
        }
      : {}),
  });
  const root = fileURLToPath(new URL(".", import.meta.url));
  const server = createServer(async (req, res) => {
    const incoming = new URL(req.url ?? "/", origin),
      notify = incoming.pathname.match(
        /^\/api\/payments\/(sandbox|stripe|oen|epay)\/notify$/,
      );
    if (notify) {
      try {
        if (req.headers.host !== new URL(applicationOrigin).host)
          throw new RouterError("invalid_host", 403);
        if (
          req.method !== "POST" &&
          !(req.method === "GET" && notify[1] === "epay")
        )
          throw new RouterError("method_not_allowed", 405);
        const gateway = gateways[notify[1]!];
        if (!gateway) throw new RouterError("resource_not_found", 404);
        let size = 0;
        const chunks: Buffer[] = [];
        for await (const chunk of req) {
          size += chunk.length;
          if (size > 65536) throw new RouterError("body_too_large", 413);
          chunks.push(Buffer.from(chunk));
        }
        const headers = Object.fromEntries(
          Object.entries(req.headers).filter(([, v]) => typeof v === "string"),
        ) as Record<string, string>;
        const raw =
          req.method === "GET"
            ? Buffer.from((req.url ?? "").split("?").slice(1).join("?"))
            : Buffer.concat(chunks);
        if (raw.length > 65536) throw new RouterError("body_too_large", 413);
        const ack = await control.hostedNotification(
          gateway.integration,
          raw,
          headers,
        );
        res.writeHead(200, {
          "Content-Type": "text/plain; charset=utf-8",
          "Cache-Control": "no-store",
        });
        res.end(ack);
      } catch (error) {
        const e = error as RouterError;
        sendJSON(res, e.status ?? 400, {
          error: e.code ?? "invalid_notification",
        });
      }
      return;
    }
    if (await bff.handle(req, res)) return;
    const path = new URL(req.url ?? "/", origin).pathname;
    if (
      req.headers.host !== new URL(applicationOrigin).host ||
      req.method !== "GET"
    ) {
      sendJSON(res, 403, { error: "invalid_request" });
      return;
    }
    const file =
      path === "/"
        ? "index.html"
        : /^\/assets\/[a-zA-Z0-9_.-]+$/.test(path)
          ? path.slice(1)
          : null;
    if (!file) {
      sendJSON(res, 404, { error: "resource_not_found" });
      return;
    }
    try {
      const data = await readFile(root + "dist/" + file);
      res.writeHead(200, {
        "Content-Type": file.endsWith(".css")
          ? "text/css; charset=utf-8"
          : file.endsWith(".js")
            ? "text/javascript; charset=utf-8"
            : "text/html; charset=utf-8",
        "Cache-Control": "no-store",
        "X-Content-Type-Options": "nosniff",
        "Referrer-Policy": "no-referrer",
        "Content-Security-Policy":
          "default-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; base-uri 'none'; frame-ancestors 'none'",
      });
      res.end(data);
    } catch {
      sendJSON(res, 404, { error: "build_required" });
    }
  });
  const checkout = createServer(async (req, res) => {
    try {
      if (
        req.headers.host !== new URL(checkoutOrigin).host ||
        !["127.0.0.1", "::1"].includes(req.socket.remoteAddress ?? "")
      )
        throw new RouterError("local_only", 403);
      const url = new URL(req.url ?? "/", checkoutOrigin);
      if (req.method === "GET" && url.pathname === "/checkout.js") {
        res.writeHead(200, {
          "Content-Type": "text/javascript; charset=utf-8",
          "Cache-Control": "no-store",
          "X-Content-Type-Options": "nosniff",
        });
        res.end(
          `document.getElementById('pay').onclick=async()=>{const b=document.getElementById('pay');b.disabled=true;try{const r=await fetch('/complete',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({ticket:new URL(location.href).searchParams.get('ticket'),provider:new URL(location.href).searchParams.get('provider')??'sandbox'})});if(!r.ok)throw Error();const result=await r.json();location.assign(result.redirect_url);}catch{document.getElementById('status').textContent='通知尚未確認，請重試。';b.disabled=false;}};`,
        );
        return;
      }
      if (req.method === "GET" && url.pathname === "/checkout") {
        const gateway = gateways[url.searchParams.get("provider") ?? "sandbox"];
        const simulator =
          gateway?.fixture ??
          (gateway?.mode === "sandbox"
            ? (gateway.adapter as LocalSandboxPayments)
            : undefined);
        if (!simulator) throw new RouterError("simulation_disabled", 403);
        const ticket = url.searchParams.get("ticket") ?? "",
          order = simulator.ticket(ticket),
          p = await bff.principal(req);
        const owned = await control.customerOrder(host, p, order.id);
        verifySnapshot(order, owned);
        if (owned.integration !== gateway!.integration)
          throw new RouterError("invalid_ticket", 403);
        if (owned.state !== "pending") {
          res.writeHead(303, {
            Location: origin + "/?order=" + encodeURIComponent(owned.id),
          });
          res.end();
          return;
        }
        // Signed base64url/hex ticket and validated order fields only; no user HTML.
        res.writeHead(200, {
          "Content-Type": "text/html; charset=utf-8",
          "Cache-Control": "no-store",
          "Content-Security-Policy":
            "default-src 'none'; style-src 'unsafe-inline'; script-src 'self'; connect-src 'self'; base-uri 'none'; frame-ancestors 'none'",
          "Referrer-Policy": "no-referrer",
        });
        res.end(
          `<!doctype html><html lang="zh-Hant"><meta name="viewport" content="width=device-width,initial-scale=1"><title>沙盒付款 · SuperRouter</title><style>body{font:16px system-ui;background:#f3f4f1;color:#1c2926;padding:24px}main{max-width:430px;margin:10vh auto;background:white;padding:36px;border-radius:22px}small{color:#52635d}h1{font-size:27px}button,a{font:inherit;display:block;margin:20px 0;background:#175c48;color:white;padding:14px;border:0;border-radius:10px;text-align:center;cursor:pointer}a{background:#eef1ee;color:#23382f;text-decoration:none}</style><main><small>${escapeHTML(gateway!.name)} · LOCAL FIXTURE · 無真實扣款</small><h1>確認你的方案付款</h1><p>商戶：SuperRouter 本機示範</p><h2>${escapeHTML(owned.currency)} ${escapeHTML(formatMinor(owned.amount_minor))}</h2><p>固定 30 天，手動續期。此為獨立沙盒付款頁，不接收信用卡。</p><button id="pay">模擬付款並送出可信通知</button><a href="${escapeHTML(origin + "/?order=" + encodeURIComponent(owned.id))}">稍後付款，返回應用</a><p id="status" role="status"></p></main><script src="/checkout.js"></script></html>`,
        );
        return;
      }
      if (req.method === "POST" && url.pathname === "/complete") {
        if (req.headers.origin !== checkoutOrigin)
          throw new RouterError("origin_denied", 403);
        if (
          !String(req.headers["content-type"] ?? "").startsWith(
            "application/json",
          )
        )
          throw new RouterError("json_required", 415);
        const data = await readJSON(req);
        if (
          Object.keys(data).some((k) => k !== "ticket" && k !== "provider") ||
          typeof data.ticket !== "string"
        )
          throw new RouterError("invalid_request");
        const gateway =
          gateways[
            typeof data.provider === "string" ? data.provider : "sandbox"
          ];
        const simulator =
          gateway?.fixture ??
          (gateway?.mode === "sandbox"
            ? (gateway.adapter as LocalSandboxPayments)
            : undefined);
        if (!simulator) throw new RouterError("simulation_disabled", 403);
        const order = simulator.ticket(data.ticket),
          p = await bff.principal(req);
        const owned = await control.customerOrder(host, p, order.id);
        verifySnapshot(order, owned);
        if (owned.integration !== gateway!.integration)
          throw new RouterError("invalid_ticket", 403);
        const notification = simulator.complete(data.ticket);
        const notifyURL = origin + "/api/payments/" + gateway!.id + "/notify";
        const delivered = await fetch(
          gateway!.id === "epay"
            ? notifyURL + "?" + notification.raw.toString()
            : notifyURL,
          gateway!.id === "epay"
            ? { method: "GET" }
            : {
                method: "POST",
                headers: notification.headers,
                body: notification.raw.toString(),
              },
        );
        if (!delivered.ok)
          throw new RouterError("notification_delivery_failed", 502);
        sendJSON(res, 200, {
          accepted: true,
          redirect_url: origin + "/?order=" + encodeURIComponent(owned.id),
        });
        return;
      }
      throw new RouterError("resource_not_found", 404);
    } catch (e) {
      const error = e as RouterError;
      sendJSON(res, error.status ?? 400, {
        error: error.code ?? "invalid_request",
      });
    }
  });
  try {
    await new Promise<void>((r, j) => {
      server.once("error", j);
      server.listen(port, "127.0.0.1", r);
    });
    await new Promise<void>((r, j) => {
      checkout.once("error", j);
      checkout.listen(checkoutPort, "127.0.0.1", r);
    });
  } catch (error) {
    bff.close();
    await Promise.all([
      new Promise<void>((r) => server.close(() => r())),
      new Promise<void>((r) => checkout.close(() => r())),
    ]);
    await store.close();
    throw error;
  }
  if (!options.quiet)
    console.log(
      `Paid chat: ${applicationOrigin} · ${paymentMode === "fixture" ? "LOCAL payment fixtures; no real charges" : environment === "live" ? "LIVE merchant payments; real charges enabled" : "TEST merchant payments"} / MOCK models`,
    );
  return {
    origin,
    publicOrigin: applicationOrigin,
    checkoutOrigin,
    store,
    control,
    host,
    bff,
    adapter,
    integration,
    mock,
    gateways,
    close: async () => {
      bff.close();
      await Promise.all([
        new Promise<void>((r) => server.close(() => r())),
        new Promise<void>((r) => checkout.close(() => r())),
      ]);
      await store.close();
    },
  };
}
function verifySnapshot(
  ticket: { id: string; amount_minor: string; currency: string },
  owned: { id: string; amount_minor: string; currency: string },
) {
  if (
    ticket.id !== owned.id ||
    ticket.amount_minor !== owned.amount_minor ||
    ticket.currency !== owned.currency
  )
    throw new RouterError("invalid_ticket", 403);
}
function escapeHTML(value: string) {
  return value.replace(
    /[&<>"']/g,
    (ch) =>
      ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[
        ch
      ]!,
  );
}
function formatMinor(value: string) {
  const n = BigInt(value);
  return `${n / 100n}.${(n % 100n).toString().padStart(2, "0")}`;
}
if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const app = await startPaidChat();
  const stop = async () => {
    await app.close();
    process.exit(0);
  };
  process.once("SIGINT", stop);
  process.once("SIGTERM", stop);
}
