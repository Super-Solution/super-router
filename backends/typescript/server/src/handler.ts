import { createHash, timingSafeEqual } from "node:crypto";
import {
  Router,
  route,
  publishCreditRateVersion,
  type DataStore,
} from "@supersolutions/super-router-core";
import {
  Invocation,
  RouterError,
  type Principal,
} from "@supersolutions/super-router-contracts";
import type { ControlPlane } from "@supersolutions/super-router-storage-postgres";

const MAX_BODY_BYTES = 256_000;
/** Inference bodies carry tool schemas, tool results and images. Only read after the caller has authenticated. */
const MAX_INFERENCE_BODY_BYTES = 4_000_000;
/**
 * Read a request body of at most MAX_BODY_BYTES. The declared length is checked first, and the stream is read
 * chunk by chunk and cancelled the moment it passes the limit, so a chunked body with no Content-Length cannot make
 * the process buffer more than the limit (this runs before authentication on payment notifications).
 */
async function rawBody(
  request: Request,
  limit = MAX_BODY_BYTES,
): Promise<Buffer> {
  const declared = Number(request.headers.get("content-length") ?? 0);
  if (declared > limit) throw new RouterError("body_too_large", 413);
  if (!request.body) return Buffer.alloc(0);
  const reader = request.body.getReader();
  const chunks: Uint8Array[] = [];
  let total = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    total += value.length;
    if (total > limit) {
      await reader.cancel().catch(() => undefined);
      throw new RouterError("body_too_large", 413);
    }
    chunks.push(value);
  }
  return Buffer.concat(chunks);
}
async function body(request: Request, limit?: number): Promise<any> {
  try {
    return JSON.parse((await rawBody(request, limit)).toString());
  } catch (e) {
    if (e instanceof RouterError) throw e;
    throw new RouterError("invalid_json");
  }
}
const hash = (s: string) => createHash("sha256").update(s).digest();
const json = (
  status: number,
  data: unknown,
  headers: Record<string, string> = {},
) =>
  new Response(JSON.stringify(data), {
    status,
    headers: { "content-type": "application/json", ...headers },
  });
const sse = (e: { type: string }) =>
  `event: ${e.type}\ndata: ${JSON.stringify(e)}\n\n`;

/**
 * Run one inference. Non-streaming requests answer with JSON. Streaming requests answer with SSE, but only
 * once the request exists: failures before that point are ordinary JSON errors with a real status code.
 */
async function infer(
  router: Router,
  principal: Principal,
  input: Invocation,
  key: string,
  signal: AbortSignal,
): Promise<Response> {
  if (!input.stream) {
    const output = await router.infer(principal, input, key, undefined, signal);
    if ("replayed" in output)
      return new Response(null, {
        status: 303,
        headers: { location: "/v1/requests/" + output.id },
      });
    return json(200, output, { "x-request-id": output.id });
  }
  const abort = new AbortController();
  const forward = () => abort.abort();
  signal.addEventListener("abort", forward, { once: true });
  const queue: string[] = [];
  let requestId: string | undefined;
  let finished = false;
  let wake: (() => void) | undefined;
  let drained: (() => void) | undefined;
  let started!: () => void;
  const first = new Promise<"event">(
    (resolve) => (started = () => resolve("event")),
  );
  const notify = () => {
    wake?.();
    wake = undefined;
  };
  const run = router
    .infer(
      principal,
      input,
      key,
      async (e) => {
        requestId = e.request_id;
        queue.push(sse(e));
        started();
        notify();
        // Backpressure: do not let a slow reader make the router buffer without bound.
        while (queue.length > 16 && !abort.signal.aborted)
          await new Promise<void>((resolve) => (drained = resolve));
      },
      abort.signal,
    )
    .then(
      (output) => ({ output }) as const,
      (error: unknown) => ({ error }) as const,
    );
  const early = await Promise.race([first, run]);
  if (early !== "event") {
    signal.removeEventListener("abort", forward);
    if ("error" in early) throw early.error;
    if ("replayed" in early.output)
      return new Response(null, {
        status: 303,
        headers: { location: "/v1/requests/" + early.output.id },
      });
    return json(200, early.output, { "x-request-id": early.output.id });
  }
  void run.then((result) => {
    if ("error" in result) {
      const failure = toError(result.error);
      queue.push(
        sse({
          type: "request.failed",
          request_id: requestId,
          error: { code: failure.code },
        } as any),
      );
    }
    finished = true;
    signal.removeEventListener("abort", forward);
    notify();
  });
  const encoder = new TextEncoder();
  return new Response(
    new ReadableStream<Uint8Array>({
      async pull(controller) {
        for (;;) {
          const chunk = queue.shift();
          if (chunk !== undefined) {
            drained?.();
            drained = undefined;
            controller.enqueue(encoder.encode(chunk));
            return;
          }
          if (finished) return controller.close();
          await new Promise<void>((resolve) => (wake = resolve));
        }
      },
      cancel() {
        abort.abort();
        drained?.();
      },
    }),
    {
      status: 200,
      headers: {
        "content-type": "text/event-stream",
        "cache-control": "no-cache",
        "x-request-id": requestId ?? "",
      },
    },
  );
}
function toError(e: unknown): RouterError {
  if (e instanceof RouterError) return e;
  const invalid = e instanceof Error && e.name === "ZodError";
  return new RouterError(
    invalid ? "invalid_request" : "internal_error",
    invalid ? 400 : 500,
  );
}
export interface RouterHandlerOptions {
  router: Router;
  /** Storage for the data plane. Any {@link DataStore}: memory, PostgreSQL or your own. */
  store: DataStore;
  /** Static API keys (self-host), or the control plane (cloud: database-backed keys, payments, admin API). */
  keys: { key: string; principal: Principal }[] | ControlPlane;
  mode?: "cloud" | "self-host";
}
/**
 * The whole HTTP API as a fetch-style handler. Mount it on anything that can pass a web `Request`:
 * `node:http`, Express and Fastify via `@supersolutions/super-router-fetch-adapters`, or Hono, Next.js, Workers,
 * Deno and Bun directly.
 */
export function createRouterHandler(
  options: RouterHandlerOptions,
): (request: Request) => Promise<Response> {
  const { router, store, keys, mode = "self-host" } = options;
  if (mode === "cloud" && Array.isArray(keys))
    throw new Error("cloud_requires_control_plane_authentication");
  const control = Array.isArray(keys) ? undefined : keys;
  const credentials = (Array.isArray(keys) ? keys : []).map((k) => ({
    digest: hash(k.key),
    principal: k.principal,
  }));
  return async (req) => {
    let requestId: string | undefined;
    try {
      const url = new URL(req.url);
      const path = url.pathname;
      const hosted = /^\/v1\/integrations\/payments\/([^/]+)\/notify$/.exec(
        path,
      );
      if (
        control &&
        hosted &&
        (req.method === "POST" || req.method === "GET")
      ) {
        const headers = Object.fromEntries(req.headers.entries());
        const raw =
          req.method === "GET"
            ? Buffer.from(url.search.slice(1))
            : await rawBody(req);
        const ack = await control.hostedNotification(
          decodeURIComponent(hosted[1]!),
          raw,
          headers,
        );
        return new Response(ack, {
          status: 200,
          headers: { "content-type": "text/plain" },
        });
      }
      const payment = /^\/v1\/integrations\/payments\/([^/]+)\/events$/.exec(
        path,
      );
      if (control && req.method === "POST" && payment) {
        const result = await control.payment(
          decodeURIComponent(payment[1]!),
          await rawBody(req),
          req.headers.get("x-payment-timestamp") ?? "",
          req.headers.get("x-payment-signature") ?? "",
        );
        return json(202, result);
      }
      const token = (req.headers.get("authorization") ?? "").replace(
        /^Bearer /,
        "",
      );
      const principal = control
        ? await control.authenticate(token)
        : token === ""
          ? undefined // an empty configured key (an unset variable) must not match "no credentials"
          : credentials.find((k) => timingSafeEqual(k.digest, hash(token)))
              ?.principal;
      if (!principal) throw new RouterError("invalid_key", 401);
      if (req.method === "GET" && path === "/v1/me")
        // What the presented key may do. Lets an app learn which customer a key is bound to.
        return json(200, {
          key_id: principal.auth_key_id ?? null,
          customer_id: principal.customer,
          scopes: principal.scopes,
          service_ids: principal.service_ids ?? null,
        });
      const key = req.headers.get("idempotency-key") ?? "";
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
            return json(
              200,
              checkout[2] === "checkout"
                ? await control.checkout(principal, id)
                : await control.reconcilePayment(
                    principal,
                    id,
                    (await body(req)).reference,
                  ),
            );
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
          if (routes[path]) return json(201, await routes[path]!());
          const km = /^\/v1\/admin\/keys\/([^/]+)\/(revoke|rotate)$/.exec(path);
          if (km) {
            const id = decodeURIComponent(km[1]!);
            return json(
              200,
              km[2] === "revoke"
                ? await control.revokeKey(principal, id, key)
                : await control.rotateKey(principal, id, key),
            );
          }
          const sm =
            /^\/v1\/admin\/subscriptions\/([^/]+)\/(cancel|plan|payment-orders)$/.exec(
              path,
            );
          if (sm) {
            const id = decodeURIComponent(sm[1]!);
            if (sm[2] === "cancel")
              return json(
                200,
                await control.cancelSubscription(principal, id, key),
              );
            if (sm[2] === "plan")
              return json(
                200,
                await control.schedulePlan(principal, id, await body(req), key),
              );
            const b = await body(req);
            if (
              !b ||
              typeof b.integration_id !== "string" ||
              Object.keys(b).some((x) => x !== "integration_id")
            )
              throw new RouterError("invalid_request");
            return json(
              201,
              await control.createOrder(principal, id, b.integration_id, key),
            );
          }
        }
        if (req.method === "GET") {
          const sub = /^\/v1\/admin\/subscriptions\/([^/]+)$/.exec(path);
          if (sub)
            return json(
              200,
              await control.getSubscription(
                principal,
                decodeURIComponent(sub[1]!),
              ),
            );
          const balance = /^\/v1\/admin\/customers\/([^/]+)\/balance$/.exec(
            path,
          );
          if (balance) {
            const id = decodeURIComponent(balance[1]!);
            return json(200, {
              data: await control.customerBalance(principal, id),
              buckets: await control.buckets(principal, id),
            });
          }
          if (path === "/v1/credits")
            return json(200, { buckets: await control.buckets(principal) });
          if (path === "/v1/windows")
            return json(200, { windows: await control.windows(principal) });
          const windows = /^\/v1\/admin\/customers\/([^/]+)\/windows$/.exec(
            path,
          );
          if (windows)
            return json(200, {
              windows: await control.windows(
                principal,
                decodeURIComponent(windows[1]!),
              ),
            });
          if (path === "/v1/admin/audit-events")
            return json(
              200,
              await control.auditLog(
                principal,
                Number(url.searchParams.get("limit") ?? 50),
                url.searchParams.get("cursor") ?? undefined,
              ),
            );
        }
      }
      if (req.method === "GET" && path === "/v1/services") {
        if (!principal.scopes.includes("invoke"))
          throw new RouterError("scope_denied", 403);
        const services = await store.list(principal);
        return json(200, {
          data: services.map((s) => ({
            id: s.id,
            version: s.version,
            tier: s.tier,
            max_output_tokens: s.max_output_tokens,
            price: s.price,
          })),
          next_cursor: null,
        });
      }
      if (req.method === "POST" && path === "/v1/inferences") {
        const input = Invocation.parse(
          await body(req, MAX_INFERENCE_BODY_BYTES),
        );
        return await infer(router, principal, input, key, req.signal);
      }
      const match = /^\/v1\/requests\/([^/]+)(\/cancel)?$/.exec(path);
      if (match) {
        const id = decodeURIComponent(match[1]!);
        if (req.method === "POST" && match[2]) {
          if (!principal.scopes.includes("invoke"))
            throw new RouterError("scope_denied", 403);
          await store.cancel(principal, id);
          return json(202, { id, cancel_requested: true });
        }
        if (req.method === "GET" && !match[2]) {
          if (!principal.scopes.includes("invoke"))
            throw new RouterError("scope_denied", 403);
          const r = await store.get(principal, id);
          return json(200, {
            id: r.id,
            execution_status: r.execution,
            billing_status: r.billing,
            service_version: r.service.version,
          });
        }
      }
      if (req.method === "GET" && path === "/v1/admin/requests") {
        if (!store.recent) throw new RouterError("not_supported", 501);
        const params = new URL(req.url).searchParams;
        return json(200, {
          data: await store.recent(
            principal,
            Number(params.get("limit") ?? 50),
            params.get("customer_id") ?? undefined,
          ),
        });
      }
      if (req.method === "GET" && path === "/v1/admin/balance") {
        if (!principal.scopes.includes("billing:read"))
          throw new RouterError("scope_denied", 403);
        return json(200, { data: await store.balance(principal) });
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
        return json(
          201,
          await publishCreditRateVersion(
            store,
            principal,
            decodeURIComponent(creditRateRoute[1]!),
            data.expected_version,
            data.config,
            mode,
          ),
        );
      }
      if (req.method === "POST" && path === "/v1/admin/service-versions")
        return json(201, await store.publish(principal, await body(req), mode));
      if (req.method === "POST" && path === "/v1/admin/routing/dry-run") {
        if (!principal.scopes.includes("routing:write"))
          throw new RouterError("scope_denied", 403);
        const input = Invocation.parse(
          await body(req, MAX_INFERENCE_BODY_BYTES),
        );
        const s = await store.getService(
          principal,
          input.service,
          input.service_version,
        );
        const d = route(s, input);
        return json(200, {
          excluded: d.excluded,
          candidates: d.candidates.map((c) => ({
            id: c.offering.id,
            score: c.score,
            cost: c.cost.toString(),
          })),
        });
      }
      throw new RouterError("resource_not_found", 404);
    } catch (e) {
      const error = toError(e);
      return json(error.status, {
        error: {
          code: error.code,
          retryable: error.retryable,
          request_id: error.request_id ?? requestId,
          ...(error.details ? { details: error.details } : {}),
        },
      });
    }
  };
}
