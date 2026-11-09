// Developer console back end. It holds the workspace owner key and talks to the router's
// public admin API on the developer's behalf. The key never reaches the browser.
import { createServer } from "node:http";
import { once } from "node:events";
import { randomUUID } from "node:crypto";
import { fileURLToPath } from "node:url";
import {
  APP_MEMBER,
  APP_SCOPES,
  CREDIT_ASSET,
  HOST,
  PORTS,
  ROUTER_URL,
  SERVICE_ID,
  creditsToAtoms,
} from "@supersolutions/example-superchat-shared";
import {
  readState,
  updateState,
} from "@supersolutions/example-superchat-shared/state";
import {
  HttpError,
  assertLocal,
  readJSON,
  routerFetch,
  send,
  serveStatic,
} from "@supersolutions/example-superchat-shared/http";

type ServiceInfo = {
  id: string;
  version: number;
  price: Record<string, string | number>;
};
const ID = /^[a-z0-9][a-z0-9_-]{1,40}$/;
const DAY = 86_400_000;

export async function startConsole(options: { quiet?: boolean } = {}) {
  const state = await readState();
  if (!state)
    throw new Error("Start the router first: it creates the local workspace.");
  const owner = state.owner_key;
  const call = <T>(path: string, init?: Parameters<typeof routerFetch>[3]) =>
    routerFetch<T>(ROUTER_URL, owner, path, init);
  const dist = fileURLToPath(new URL("./dist/", import.meta.url));

  const routes: Record<string, (body: any) => Promise<unknown>> = {
    "GET /api/overview": async () => {
      const s = (await readState())!;
      const [services, scenario] = await Promise.all([
        call<{ data: ServiceInfo[] }>("/v1/services"),
        call<{ scenario: string; scenarios: unknown[] }>("/__dev/scenario"),
      ]);
      const customers = await Promise.all(
        s.customers.map(async (id) => {
          const b = await call<{
            buckets: { asset: string; available: string }[];
          }>(`/v1/admin/customers/${encodeURIComponent(id)}/balance`);
          const available = b.buckets
            .filter((x) => x.asset === CREDIT_ASSET)
            .reduce((sum, x) => sum + BigInt(x.available), 0n);
          return { id, available: available.toString() };
        }),
      );
      return {
        workspace: s.workspace,
        router: ROUTER_URL,
        service: services.data.find((x) => x.id === SERVICE_ID) ?? null,
        customers,
        keys: s.keys,
        ...scenario,
      };
    },
    "GET /api/requests": async () => call("/v1/admin/requests?limit=40"),
    "POST /api/customers": async (b) => {
      if (typeof b.id !== "string" || !ID.test(b.id))
        throw new HttpError(400, "invalid_customer_id");
      await call("/v1/admin/customers", {
        method: "POST",
        body: { id: b.id, external_id: b.id },
        idempotencyKey: "customer:" + b.id,
      });
      await updateState((s) => {
        if (!s.customers.includes(b.id)) s.customers.push(b.id);
      });
      return { id: b.id };
    },
    "POST /api/grants": async (b) => {
      if (typeof b.customer_id !== "string" || typeof b.credits !== "string")
        throw new HttpError(400, "invalid_request");
      let amount: string;
      try {
        amount = creditsToAtoms(b.credits);
      } catch {
        throw new HttpError(400, "invalid_credits");
      }
      if (amount === "0") throw new HttpError(400, "invalid_credits");
      return call("/v1/admin/credits/grants", {
        method: "POST",
        body: {
          customer_id: b.customer_id,
          asset: CREDIT_ASSET,
          amount,
          reason: "Top-up from the local developer console",
        },
        idempotencyKey:
          typeof b.idempotency_key === "string"
            ? b.idempotency_key
            : randomUUID(),
      });
    },
    "POST /api/keys": async (b) => {
      if (typeof b.customer_id !== "string")
        throw new HttpError(400, "invalid_request");
      const issued = await call<{
        id: string;
        token: string;
        expires_at: string;
      }>("/v1/admin/keys", {
        method: "POST",
        body: {
          member_id: APP_MEMBER,
          customer_id: b.customer_id,
          scopes: APP_SCOPES,
          service_ids: [SERVICE_ID],
          expires_at: new Date(Date.now() + 30 * DAY).toISOString(),
        },
        idempotencyKey: randomUUID(),
      });
      await updateState((s) => {
        s.keys.unshift({
          id: issued.id,
          customer_id: b.customer_id,
          token_hint: issued.token.slice(0, 7) + "…" + issued.token.slice(-4),
          created_at: new Date().toISOString(),
          expires_at: issued.expires_at,
        });
      });
      // The full token is shown once and only stored by the router as a hash.
      return issued;
    },
    "POST /api/rate": async (b) => {
      const input = String(b.input_tokens_per_credit ?? "");
      const output = String(b.output_tokens_per_credit ?? "");
      if (!/^[1-9]\d{0,9}$/.test(input) || !/^[1-9]\d{0,9}$/.test(output))
        throw new HttpError(400, "invalid_rate");
      const services = await call<{ data: ServiceInfo[] }>("/v1/services");
      const current = services.data.find((x) => x.id === SERVICE_ID);
      if (!current) throw new HttpError(404, "service_missing");
      return call(`/v1/admin/services/${SERVICE_ID}/credit-rates`, {
        method: "POST",
        body: {
          expected_version: current.version,
          config: {
            asset: CREDIT_ASSET,
            tokens: {
              input: { units: input },
              cached: { units: String(BigInt(input) * 2n) },
              cache_write: { units: input },
              output: { units: output },
            },
          },
        },
        idempotencyKey: randomUUID(),
      });
    },
    "POST /api/scenario": async (b) =>
      call("/__dev/scenario", {
        method: "POST",
        body: { scenario: b.scenario },
        idempotencyKey: randomUUID(),
      }),
  };

  const server = createServer(async (req, res) => {
    const url = new URL(req.url ?? "/", "http://localhost");
    try {
      assertLocal(req, PORTS.console);
      const revoke = /^\/api\/keys\/([^/]+)\/revoke$/.exec(url.pathname);
      if (req.method === "POST" && revoke) {
        const id = decodeURIComponent(revoke[1]!);
        await call(`/v1/admin/keys/${encodeURIComponent(id)}/revoke`, {
          method: "POST",
          idempotencyKey: "revoke:" + id,
        });
        await updateState((s) => {
          const k = s.keys.find((x) => x.id === id);
          if (k && !k.revoked_at) k.revoked_at = new Date().toISOString();
        });
        return send(res, 200, { id, revoked: true });
      }
      const route = routes[`${req.method} ${url.pathname}`];
      if (route)
        return send(
          res,
          200,
          await route(req.method === "POST" ? await readJSON(req) : {}),
        );
      if (url.pathname.startsWith("/api/"))
        throw new HttpError(404, "not_found");
      await serveStatic(res, dist, url.pathname);
    } catch (error) {
      const e =
        error instanceof HttpError
          ? error
          : new HttpError(500, "internal_error");
      if (!(error instanceof HttpError)) console.error("[console]", error);
      send(res, e.status, { error: { code: e.code } });
    }
  });
  server.listen(PORTS.console, HOST);
  await once(server, "listening");
  if (!options.quiet) console.log(`[console] http://${HOST}:${PORTS.console}`);
  return {
    server,
    close: () => new Promise<void>((r) => server.close(() => r())),
  };
}
