import {
  createServer,
  type IncomingMessage,
  type ServerResponse,
} from "node:http";
import { createHash, timingSafeEqual } from "node:crypto";
import {
  Router,
  route,
  publishCreditRateVersion,
} from "@supersolutions/super-router-core";
import {
  Invocation,
  RouterError,
  type Principal,
} from "@supersolutions/super-router-contracts";
import {
  PostgresStore,
  ControlPlane,
} from "@supersolutions/super-router-storage-postgres";
import { once } from "node:events";
async function rawBody(req: IncomingMessage) {
  let bytes = 0;
  const parts: Buffer[] = [];
  for await (const part of req) {
    bytes += part.length;
    if (bytes > 256000) throw new RouterError("body_too_large", 413);
    parts.push(part);
  }
  return Buffer.concat(parts);
}
async function body(req: IncomingMessage) {
  try {
    return JSON.parse((await rawBody(req)).toString());
  } catch {
    throw new RouterError("invalid_json");
  }
}
const hash = (s: string) => createHash("sha256").update(s).digest();
export function createRouterServer(
  router: Router,
  store: PostgresStore,
  keys: { key: string; principal: Principal }[] | ControlPlane,
  mode: "cloud" | "self-host" = "self-host",
) {
  if (mode === "cloud" && Array.isArray(keys))
    throw new Error("cloud_requires_control_plane_authentication");
  const control = Array.isArray(keys) ? undefined : keys;
  const credentials = (Array.isArray(keys) ? keys : []).map((k) => ({
    digest: hash(k.key),
    principal: k.principal,
  }));
  return createServer(async (req, res) => {
    const json = (status: number, data: unknown) => {
      res.writeHead(status, { "content-type": "application/json" });
      res.end(JSON.stringify(data));
    };
    let requestId: string | undefined;
    try {
      const path = new URL(req.url ?? "/", "http://localhost").pathname;
      const hosted = /^\/v1\/integrations\/payments\/([^/]+)\/notify$/.exec(
        path,
      );
      if (
        control &&
        hosted &&
        (req.method === "POST" || req.method === "GET")
      ) {
        const headers = Object.fromEntries(
          Object.entries(req.headers).map(([k, v]) => [k, String(v ?? "")]),
        );
        const raw =
          req.method === "GET"
            ? Buffer.from(new URL(req.url!, "http://localhost").search.slice(1))
            : await rawBody(req);
        const ack = await control.hostedNotification(
          decodeURIComponent(hosted[1]!),
          raw,
          headers,
        );
        res.writeHead(200, { "content-type": "text/plain" });
        res.end(ack);
        return;
      }
      const payment = /^\/v1\/integrations\/payments\/([^/]+)\/events$/.exec(
        path,
      );
      if (control && req.method === "POST" && payment) {
        const result = await control.payment(
          decodeURIComponent(payment[1]!),
          await rawBody(req),
          String(req.headers["x-payment-timestamp"] ?? ""),
          String(req.headers["x-payment-signature"] ?? ""),
        );
        json(202, result);
        return;
      }
      const token = (req.headers.authorization ?? "").replace(/^Bearer /, "");
      const principal = control
        ? await control.authenticate(token)
        : credentials.find((k) => timingSafeEqual(k.digest, hash(token)))
            ?.principal;
      if (!principal) throw new RouterError("invalid_key", 401);
      const key = String(req.headers["idempotency-key"] ?? "");
      if (req.method === "POST" && !key)
        throw new RouterError("missing_idempotency_key");
      if (control) {
        if (req.method === "POST") {
          const checkout =
            /^\/v1\/admin\/payment-orders\/([^/]+)\/(checkout|reconcile)$/.exec(
              path,
            );
          if (checkout) {
            const id = decodeURIComponent(checkout[1]!);
            json(
              200,
              checkout[2] === "checkout"
                ? await control.checkout(principal, id)
                : await control.reconcilePayment(
                    principal,
                    id,
                    (await body(req)).reference,
                  ),
            );
            return;
          }
          const routes: Record<string, () => Promise<unknown>> = {
            "/v1/admin/members": async () =>
              control.member(principal, await body(req), key),
            "/v1/admin/customers": async () =>
              control.customer(principal, await body(req), key),
            "/v1/admin/keys": async () =>
              control.issueKey(principal, await body(req), key),
            "/v1/admin/plans": async () =>
              control.publishPlan(principal, await body(req), key),
            "/v1/admin/subscriptions": async () =>
              control.subscribe(principal, await body(req), key),
            "/v1/admin/credits/grants": async () =>
              control.grantCredits(principal, await body(req), key),
          };
          if (routes[path]) {
            json(201, await routes[path]!());
            return;
          }
          const km = /^\/v1\/admin\/keys\/([^/]+)\/(revoke|rotate)$/.exec(path);
          if (km) {
            json(
              200,
              km[2] === "revoke"
                ? await control.revokeKey(
                    principal,
                    decodeURIComponent(km[1]!),
                    key,
                  )
                : await control.rotateKey(
                    principal,
                    decodeURIComponent(km[1]!),
                    key,
                  ),
            );
            return;
          }
          const sm =
            /^\/v1\/admin\/subscriptions\/([^/]+)\/(cancel|plan|payment-orders)$/.exec(
              path,
            );
          if (sm) {
            const id = decodeURIComponent(sm[1]!);
            if (sm[2] === "cancel") {
              json(200, await control.cancelSubscription(principal, id, key));
              return;
            }
            if (sm[2] === "plan") {
              json(
                200,
                await control.schedulePlan(principal, id, await body(req), key),
              );
              return;
            }
            const b = await body(req);
            if (
              !b ||
              typeof b.integration_id !== "string" ||
              Object.keys(b).some((x) => x !== "integration_id")
            )
              throw new RouterError("invalid_request");
            json(
              201,
              await control.createOrder(principal, id, b.integration_id, key),
            );
            return;
          }
        }
        if (req.method === "GET") {
          const sub = /^\/v1\/admin\/subscriptions\/([^/]+)$/.exec(path);
          if (sub) {
            json(
              200,
              await control.getSubscription(
                principal,
                decodeURIComponent(sub[1]!),
              ),
            );
            return;
          }
          const balance = /^\/v1\/admin\/customers\/([^/]+)\/balance$/.exec(
            path,
          );
          if (balance) {
            const id = decodeURIComponent(balance[1]!);
            json(200, {
              data: await control.customerBalance(principal, id),
              buckets: await control.buckets(principal, id),
            });
            return;
          }
          if (path === "/v1/credits") {
            json(200, { buckets: await control.buckets(principal) });
            return;
          }
          if (path === "/v1/admin/audit-events") {
            const url = new URL(req.url!, "http://localhost");
            json(
              200,
              await control.auditLog(
                principal,
                Number(url.searchParams.get("limit") ?? 50),
                url.searchParams.get("cursor") ?? undefined,
              ),
            );
            return;
          }
        }
      }
      if (req.method === "GET" && path === "/v1/services") {
        if (!principal.scopes.includes("invoke"))
          throw new RouterError("scope_denied", 403);
        const services = await store.list(principal);
        json(200, {
          data: services.map((s) => ({
            id: s.id,
            version: s.version,
            tier: s.tier,
            max_output_tokens: s.max_output_tokens,
            price: s.price,
          })),
          next_cursor: null,
        });
        return;
      }
      if (req.method === "POST" && path === "/v1/inferences") {
        const input = Invocation.parse(await body(req));
        const abort = new AbortController();
        res.on("close", () => {
          if (!res.writableEnded) abort.abort();
        });
        const output = await router.infer(
          principal,
          input,
          key,
          input.stream
            ? async (e) => {
                requestId = e.request_id;
                if (!res.headersSent)
                  res.writeHead(200, {
                    "content-type": "text/event-stream",
                    "cache-control": "no-cache",
                    "x-request-id": e.request_id,
                  });
                if (res.destroyed) throw new Error("client_closed");
                if (
                  !res.write(`event: ${e.type}\ndata: ${JSON.stringify(e)}\n\n`)
                )
                  await Promise.race([
                    once(res, "drain"),
                    once(res, "close").then(() => {
                      throw new Error("client_closed");
                    }),
                  ]);
              }
            : undefined,
          abort.signal,
        );
        if ("replayed" in output) {
          res.writeHead(303, { location: "/v1/requests/" + output.id });
          res.end();
        } else if (input.stream) res.end();
        else {
          res.setHeader("x-request-id", output.id);
          json(200, output);
        }
        return;
      }
      const match = /^\/v1\/requests\/([^/]+)(\/cancel)?$/.exec(path);
      if (match) {
        const id = decodeURIComponent(match[1]!);
        if (req.method === "POST" && match[2]) {
          if (!principal.scopes.includes("invoke"))
            throw new RouterError("scope_denied", 403);
          await store.cancel(principal, id);
          json(202, { id, cancel_requested: true });
          return;
        }
        if (req.method === "GET" && !match[2]) {
          if (!principal.scopes.includes("invoke"))
            throw new RouterError("scope_denied", 403);
          const r = await store.get(principal, id);
          json(200, {
            id: r.id,
            execution_status: r.execution,
            billing_status: r.billing,
            service_version: r.service.version,
          });
          return;
        }
      }
      if (req.method === "GET" && path === "/v1/admin/balance") {
        if (!principal.scopes.includes("billing:read"))
          throw new RouterError("scope_denied", 403);
        json(200, { data: await store.balance(principal) });
        return;
      }
      const creditRateRoute =
        /^\/v1\/admin\/services\/([^/]+)\/credit-rates$/.exec(path);
      if (req.method === "POST" && creditRateRoute) {
        const data = await body(req);
        if (
          !data ||
          typeof data !== "object" ||
          Array.isArray(data) ||
          Object.keys(data).some(
            (k) => !["expected_version", "config"].includes(k),
          )
        )
          throw new RouterError("invalid_credit_rates");
        const service = await publishCreditRateVersion(
          store,
          principal,
          decodeURIComponent(creditRateRoute[1]!),
          data.expected_version,
          data.config,
          mode,
        );
        json(201, service);
        return;
      }
      if (req.method === "POST" && path === "/v1/admin/service-versions") {
        const s = await store.publish(principal, await body(req), mode);
        json(201, s);
        return;
      }
      if (req.method === "POST" && path === "/v1/admin/routing/dry-run") {
        if (!principal.scopes.includes("routing:write"))
          throw new RouterError("scope_denied", 403);
        const input = Invocation.parse(await body(req));
        const s = await store.getService(
          principal,
          input.service,
          input.service_version,
        );
        const d = route(s, input);
        json(200, {
          excluded: d.excluded,
          candidates: d.candidates.map((c) => ({
            id: c.offering.id,
            score: c.score,
            cost: c.cost.toString(),
          })),
        });
        return;
      }
      throw new RouterError("resource_not_found", 404);
    } catch (e) {
      const error =
        e instanceof RouterError
          ? e
          : new RouterError(
              e instanceof Error && e.name === "ZodError"
                ? "invalid_request"
                : "internal_error",
              e instanceof Error && e.name === "ZodError" ? 400 : 500,
            );
      if (res.headersSent) {
        if (!res.destroyed)
          res.end(
            `event: request.failed\ndata: ${JSON.stringify({ type: "request.failed", request_id: requestId, error: { code: error.code } })}\n\n`,
          );
      } else
        json(error.status, {
          error: {
            code: error.code,
            retryable: error.retryable,
            request_id: error.request_id ?? requestId,
          },
        });
    }
  });
}
