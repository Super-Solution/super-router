/** Server only. Never import this package from a browser bundle. */
import { randomBytes, createHash } from "node:crypto";
import type { IncomingMessage, ServerResponse } from "node:http";
import {
  PlanInput,
  RouterError,
  type Principal,
  type Service,
} from "@superrouter/contracts";
import type { ControlPlane } from "@superrouter/storage-postgres";
import type { Router } from "@superrouter/core";
export interface Offer {
  id: string;
  name: string;
  description: string;
  plan: PlanInput;
  payment_methods?: string[];
}
export function validatePreset(offers: Offer[]) {
  if (!offers.length || new Set(offers.map((o) => o.id)).size !== offers.length)
    throw new Error("Unique offers required");
  return offers.map((o) => ({ ...o, plan: PlanInput.parse(o.plan) }));
}
export async function publishPreset(
  control: ControlPlane,
  host: Principal,
  raw: Offer[],
) {
  const offers = validatePreset(raw);
  for (const o of offers)
    await control.publishPlan(
      host,
      o.plan,
      `preset:${o.plan.id}:${o.plan.version}`,
    );
  return offers;
}
export async function readJSON(
  req: IncomingMessage,
): Promise<Record<string, unknown>> {
  const chunks: Buffer[] = [];
  let size = 0;
  for await (const chunk of req) {
    size += chunk.length;
    if (size > 16000) throw new RouterError("body_too_large", 413);
    chunks.push(Buffer.from(chunk));
  }
  try {
    const data = JSON.parse(Buffer.concat(chunks).toString() || "{}");
    if (!data || Array.isArray(data) || typeof data !== "object") throw Error();
    return data;
  } catch {
    throw new RouterError("invalid_json");
  }
}
export function sendJSON(res: ServerResponse, status: number, value: unknown) {
  res.writeHead(status, {
    "Content-Type": "application/json; charset=utf-8",
    "Cache-Control": "no-store",
    "X-Content-Type-Options": "nosniff",
  });
  res.end(JSON.stringify(value));
}
const digest = (s: string) => createHash("sha256").update(s).digest("hex");
function displayAtoms(value: string) {
  const digits = BigInt(value).toString().padStart(10, "0"),
    fraction = digits.slice(-9).replace(/0+$/, "");
  return digits.slice(0, -9) + (fraction ? "." + fraction : "");
}
function displayRate(service: Service) {
  const p = service.price;
  if (!service.task_kind) {
    const tokenRate = (value: string, label: string) => {
      const atoms = BigInt(value),
        per = BigInt(p.per),
        numerator = 1000000000n * per;
      return atoms > 0n && numerator % atoms === 0n
        ? `${label} 1 credit＝${numerator / atoms} tokens`
        : `${label} ${displayAtoms(value)} credits／${p.per} tokens`;
    };
    return tokenRate(p.input, "輸入") + " · " + tokenRate(p.output, "輸出");
  }
  const meter =
    service.task_kind === "image"
      ? "images"
      : service.task_kind === "video"
        ? "video_ms"
        : service.task_kind === "audio"
          ? "audio_ms"
          : "tasks";
  const r = p.media?.[meter];
  if (!r) throw new RouterError("price_unavailable", 503);
  const duration = meter === "video_ms" || meter === "audio_ms";
  const units =
    duration && r.per % 1000 === 0
      ? `${r.per / 1000} 秒`
      : duration
        ? `${r.per} 毫秒`
        : meter === "images"
          ? `${r.per} 張`
          : `${r.per} 次成功`;
  return `${displayAtoms(r.atoms)} credits／${units}`;
}
export interface BillingBFFConfig {
  origin: string;
  control: ControlPlane;
  host: Principal;
  router: Router;
  offers: Offer[];
  integration: string;
  paymentMethods?: {
    id: string;
    name: string;
    mode: string;
    integration: string;
    currencies: string[];
    allowedOrigins: string[];
    capabilities: unknown;
  }[];
  allowedCheckoutOrigins: string[];
  services: Record<
    string,
    {
      id: string;
      kind?: "image" | "video" | "audio" | "task";
      unit: string;
      rate: string;
      max: number;
    }
  >;
  demoKey?: (fresh?: boolean) => Promise<string>;
  sandbox?: boolean;
  sessionMs?: number;
}
/** Host supplies login and merchant binding. Sessions are ephemeral; bearer keys never leave this process. */
export function createCustomerBillingBFF(config: BillingBFFConfig) {
  const offers = validatePreset(config.offers),
    sessions = new Map<string, { key: string; expires: number }>();
  const attempts = new Map<string, { until: number; count: number }>();
  const ttl = config.sessionMs ?? 3600000;
  const strict = (data: Record<string, unknown>, keys: string[]) => {
    if (Object.keys(data).some((k) => !keys.includes(k)))
      throw new RouterError("unexpected_field");
  };
  async function principal(req: IncomingMessage) {
    const cookie = (req.headers.cookie ?? "")
      .split(";")
      .map((v) => v.trim())
      .find((v) => v.startsWith("sr_paid_session="))
      ?.slice(16);
    const s = cookie ? sessions.get(digest(cookie)) : undefined;
    if (!s || s.expires < Date.now()) {
      if (cookie) sessions.delete(digest(cookie));
      throw new RouterError("session_required", 401);
    }
    const p = await config.control.authenticate(s.key);
    if (
      p.workspace !== config.host.workspace ||
      p.environment !== config.host.environment ||
      p.customer.startsWith("_") ||
      !p.scopes.includes("credits:read") ||
      !p.scopes.includes("invoke") ||
      p.scopes.some((s) => s.startsWith("billing:") || s === "identity:write")
    )
      throw new RouterError("customer_key_required", 403);
    return p;
  }
  async function handle(
    req: IncomingMessage,
    res: ServerResponse,
  ): Promise<boolean> {
    const path = new URL(req.url ?? "/", config.origin).pathname;
    if (!path.startsWith("/api/")) return false;
    try {
      if (req.headers.host !== new URL(config.origin).host)
        throw new RouterError("invalid_host", 403);
      const write = req.method === "POST";
      if (req.method !== "GET" && !write)
        throw new RouterError("method_not_allowed", 405);
      if (write) {
        if (req.headers.origin !== config.origin)
          throw new RouterError("origin_denied", 403);
        if (
          !String(req.headers["content-type"] ?? "").startsWith(
            "application/json",
          )
        )
          throw new RouterError("json_required", 415);
        const identity = req.socket.remoteAddress ?? "unknown";
        let a = attempts.get(identity);
        if (!a || a.until < Date.now()) {
          a = { until: Date.now() + 60000, count: 0 };
          attempts.set(identity, a);
        }
        if (++a.count > 120) throw new RouterError("rate_limited", 429);
      }
      if (path === "/api/session" && write) {
        const data = await readJSON(req);
        strict(data, ["key", "demo", "new_demo"]);
        if (
          data.new_demo !== undefined &&
          (data.new_demo !== true || data.demo !== true)
        )
          throw new RouterError("invalid_request");
        if (data.demo !== undefined && data.demo !== true)
          throw new RouterError("invalid_request");
        if (data.demo && data.key) throw new RouterError("invalid_request");
        if (
          data.demo &&
          (!config.sandbox ||
            !config.demoKey ||
            !["127.0.0.1", "::1"].includes(req.socket.remoteAddress ?? ""))
        )
          throw new RouterError("demo_disabled", 403);
        const key = data.demo
          ? await config.demoKey!(data.new_demo === true)
          : typeof data.key === "string"
            ? data.key
            : "";
        const p = await config.control.authenticate(key);
        if (
          p.workspace !== config.host.workspace ||
          p.environment !== config.host.environment ||
          p.customer.startsWith("_") ||
          !p.scopes.includes("credits:read") ||
          !p.scopes.includes("invoke") ||
          p.scopes.some(
            (s) => s.startsWith("billing:") || s === "identity:write",
          )
        )
          throw new RouterError("customer_key_required", 403);
        const token = randomBytes(32).toString("hex");
        // Bound process memory, and rotate the caller's previous session on reconnect.
        const old = (req.headers.cookie ?? "")
          .split(";")
          .map((v) => v.trim())
          .find((v) => v.startsWith("sr_paid_session="))
          ?.slice(16);
        if (old) sessions.delete(digest(old));
        for (const [k, s] of sessions)
          if (s.expires < Date.now()) sessions.delete(k);
        if (sessions.size >= 1000) throw new RouterError("sessions_full", 503);
        sessions.set(digest(token), { key, expires: Date.now() + ttl });
        res.setHeader(
          "Set-Cookie",
          `sr_paid_session=${token}; HttpOnly; SameSite=Strict; Path=/; Max-Age=${Math.floor(ttl / 1000)}${config.origin.startsWith("https:") ? "; Secure" : ""}`,
        );
        sendJSON(res, 200, { connected: true, customer: p.customer });
        return true;
      }
      if (path === "/api/catalog" && req.method === "GET") {
        const currentServices = Object.fromEntries(
          await Promise.all(
            Object.entries(config.services).map(async ([name, definition]) => {
              const service = await config.control.store.getService(
                config.host,
                definition.id,
              );
              return [
                name,
                {
                  ...definition,
                  asset: service.price.asset,
                  version: service.version,
                  rate: displayRate(service),
                },
              ];
            }),
          ),
        );
        sendJSON(res, 200, {
          offers,
          payment_methods: config.paymentMethods?.map(
            ({ integration, allowedOrigins, ...publicMethod }) => publicMethod,
          ) ?? [
            {
              id: "sandbox",
              name: "本機沙盒",
              mode: "sandbox",
              currencies: ["TWD"],
            },
          ],
          services: currentServices,
          mode: config.sandbox ? "sandbox" : "hosted",
          capabilities: {
            automatic_recurring: false,
            portal: false,
            refunds: false,
          },
        });
        return true;
      }
      if (path === "/api/logout" && write) {
        const cookie = (req.headers.cookie ?? "")
          .split(";")
          .map((v) => v.trim())
          .find((v) => v.startsWith("sr_paid_session="))
          ?.slice(16);
        if (cookie) sessions.delete(digest(cookie));
        res.setHeader(
          "Set-Cookie",
          "sr_paid_session=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0",
        );
        sendJSON(res, 200, { ok: true });
        return true;
      }
      const p = await principal(req);
      if (path === "/api/billing/me" && req.method === "GET") {
        const billing = await config.control.customerBilling(config.host, p);
        sendJSON(res, 200, {
          customer: p.customer,
          ...billing,
          orders: billing.orders.map((o) => ({
            ...o,
            payment_method:
              config.paymentMethods?.find(
                (m) => m.integration === o.integration,
              )?.id ?? "sandbox",
          })),
        });
        return true;
      }
      const match = path.match(/^\/api\/billing\/orders\/([a-zA-Z0-9-]+)$/);
      if (match && req.method === "GET") {
        sendJSON(
          res,
          200,
          await config.control.customerOrder(config.host, p, match[1]!),
        );
        return true;
      }
      if (!write) throw new RouterError("resource_not_found", 404);
      const data = await readJSON(req),
        key = String(req.headers["idempotency-key"] ?? "");
      if (!key || key.length > 160)
        throw new RouterError("missing_idempotency_key");
      if (path === "/api/billing/checkout") {
        strict(data, ["offer_id", "payment_method"]);
        const method = config.paymentMethods?.find(
          (m) =>
            m.id === (data.payment_method ?? config.paymentMethods![0]?.id),
        );
        if (config.paymentMethods && !method)
          throw new RouterError("payment_method_unavailable", 404);
        const offer = offers.find((o) => o.id === data.offer_id);
        if (!offer) throw new RouterError("offer_unavailable", 404);
        if (
          method &&
          (!method.currencies.includes(offer.plan.currency) ||
            (offer.payment_methods &&
              !offer.payment_methods.includes(method.id)))
        )
          throw new RouterError("payment_currency_mismatch", 422);
        const order = await config.control.customerCheckout(
          config.host,
          p,
          { plan_id: offer.plan.id, plan_version: offer.plan.version },
          method?.integration ?? config.integration,
          key,
        );
        const checkout = await config.control.checkout(config.host, order.id);
        const url = new URL(checkout.url);
        if (
          !(method?.allowedOrigins ?? config.allowedCheckoutOrigins).includes(
            url.origin,
          ) ||
          url.username ||
          url.password
        )
          throw new RouterError("invalid_checkout_url", 502);
        sendJSON(res, 200, { order_id: order.id, redirect_url: checkout.url });
        return true;
      }
      if (path === "/api/billing/subscription/cancel") {
        strict(data, ["subscription_id"]);
        if (typeof data.subscription_id !== "string")
          throw new RouterError("invalid_request");
        sendJSON(
          res,
          200,
          await config.control.customerCancel(
            config.host,
            p,
            data.subscription_id,
            key,
          ),
        );
        return true;
      }
      if (path === "/api/infer") {
        strict(data, [
          "mode",
          "prompt",
          "quantity",
          "expected_service_version",
        ]);
        if (
          typeof data.expected_service_version !== "number" ||
          !Number.isSafeInteger(data.expected_service_version) ||
          data.expected_service_version < 1
        )
          throw new RouterError("expected_service_version_required");
        const service = config.services[String(data.mode)];
        if (
          !service ||
          typeof data.prompt !== "string" ||
          !data.prompt.trim() ||
          data.prompt.length > 8000
        )
          throw new RouterError("invalid_request");
        const quantity = data.quantity ?? 1;
        if (
          typeof quantity !== "number" ||
          !Number.isSafeInteger(quantity) ||
          quantity < 1 ||
          quantity > service.max
        )
          throw new RouterError("invalid_quantity");
        const meter =
          service.kind === "image"
            ? "images"
            : service.kind === "video"
              ? "video_ms"
              : service.kind === "audio"
                ? "audio_ms"
                : "tasks";
        const input = service.kind
          ? { task: { kind: service.kind, prompt: data.prompt } }
          : { messages: [{ role: "user", content: data.prompt }] };
        const disconnected = new AbortController();
        const abort = () => {
          if (!res.writableEnded) disconnected.abort();
        };
        res.once("close", abort);
        try {
          const result = await config.router.infer(
            p,
            {
              service: service.id,
              service_version: data.expected_service_version,
              customer_id: p.customer,
              input,
              limits: {
                max_output_tokens: service.kind ? 1 : 64,
                ...(service.kind
                  ? {
                      max_usage: {
                        [meter]:
                          quantity *
                          (service.kind === "video" || service.kind === "audio"
                            ? 1000
                            : 1),
                      },
                    }
                  : {}),
              },
              stream: false,
            },
            `paid:${p.customer}:${key}`,
            undefined,
            disconnected.signal,
          );
          if (!res.destroyed) sendJSON(res, 200, result);
        } finally {
          res.off("close", abort);
        }
        return true;
      }
      throw new RouterError("resource_not_found", 404);
    } catch (error) {
      const e = error as {
        code?: string;
        status?: number;
        statusCode?: number;
      };
      sendJSON(res, e.status ?? e.statusCode ?? 400, {
        error:
          e.code === "inactive_service_version"
            ? "service_version_changed"
            : (e.code ?? "invalid_request"),
      });
      return true;
    }
  }
  return {
    handle,
    principal,
    close: () => {
      sessions.clear();
      attempts.clear();
    },
  };
}

// Developer/server configuration only. Never expose this setter to customer sessions.
export {
  CREDIT_ATOMS,
  CreditRateConfig,
  compileCreditPrice,
  publishCreditRateVersion,
} from "@superrouter/core";
