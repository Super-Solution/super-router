// The Super Router side of the example: a real server with a PostgreSQL ledger,
// database-backed API keys and two local mock providers.
import type { IncomingMessage, ServerResponse } from "node:http";
import { createServer } from "node:http";
import { once } from "node:events";
import { timingSafeEqual, createHash } from "node:crypto";
import { Router } from "@supersolutions/super-router-core";
import { RouterError, Service } from "@supersolutions/super-router-contracts";
import {
  ControlPlane,
  PostgresStore,
  LOCAL_DATABASE_URL,
} from "@supersolutions/super-router-storage-postgres";
import { createRouterServer } from "@supersolutions/super-router-server";
import {
  APP_MEMBER,
  CREDIT_ASSET,
  ENVIRONMENT,
  HOST,
  MAX_OUTPUT_TOKENS,
  PORTS,
  SCENARIOS,
  SEED_CUSTOMERS,
  SERVICE_ID,
  creditsToAtoms,
  isScenario,
} from "@supersolutions/example-superchat-shared";
import {
  loadOrCreateState,
  updateState,
  type LocalState,
} from "@supersolutions/example-superchat-shared/state";
import { ScenarioProvider, scenario } from "./providers.js";

const offering = (
  id: string,
  provider: string,
  input: string,
  output: string,
) => ({
  id,
  provider,
  model: "mock-chat",
  upstream_model: "mock-chat",
  region: "local",
  labels: [],
  enabled: true,
  source: "byok" as const,
  max_input_tokens: 4096,
  max_output_tokens: MAX_OUTPUT_TOKENS,
  // What the (mock) provider charges us, in USD atoms (1e-9 USD) per token: $0.20 / $0.80 per million.
  price: {
    asset: "USD",
    input,
    cached: input,
    cache_write: input,
    output,
    per: 1,
  },
});

/** The Service the chat app sells. Customer price is in credits; 1 credit = 1e9 atoms. */
export const chatService = Service.parse({
  id: SERVICE_ID,
  version: 1,
  mode: "tier",
  tier: "chat/standard",
  regions: ["local"],
  required_labels: [],
  // 0.00002 credits per input token, 0.00006 per output token.
  price: {
    asset: CREDIT_ASSET,
    input: "20000",
    cached: "10000",
    cache_write: "20000",
    output: "60000",
    per: 1,
  },
  max_attempts: 2,
  max_provider_cost: "10000000",
  max_output_tokens: MAX_OUTPUT_TOKENS,
  // Pure cost preference: the cheaper primary is tried first, the backup second.
  weights: { cost: 1, latency: 0, reliability: 0, quality: 0 },
  // Larger than either offering's worst-case cost, so cost differences actually rank them.
  cost_scale: "10000000",
  latency_scale_ms: 1000,
  offerings: [
    offering("mock-primary", "primary", "200", "800"),
    offering("mock-backup", "backup", "400", "1600"),
  ],
});

async function provision(
  control: ControlPlane,
  store: PostgresStore,
  state: LocalState,
) {
  try {
    await control.bootstrap(
      state.workspace,
      ENVIRONMENT,
      "owner",
      state.owner_key,
    );
  } catch (e) {
    if (!(e instanceof RouterError) || e.code !== "already_bootstrapped")
      throw e;
  }
  const owner = await control.authenticate(state.owner_key);
  try {
    await store.getService(owner, chatService.id);
  } catch {
    await store.publish(owner, chatService);
  }
  await control.member(
    owner,
    { id: APP_MEMBER, role: "app", active: true },
    "member:" + APP_MEMBER,
  );
  for (const c of SEED_CUSTOMERS) {
    await control.customer(
      owner,
      { id: c.id, external_id: c.id },
      "customer:" + c.id,
    );
    await control.grantCredits(
      owner,
      {
        customer_id: c.id,
        asset: CREDIT_ASSET,
        amount: creditsToAtoms(c.credits),
        reason: "Starting credits for the local example",
      },
      "welcome:" + c.id,
    );
  }
  await updateState((s) => {
    for (const c of SEED_CUSTOMERS)
      if (!s.customers.includes(c.id)) s.customers.push(c.id);
  });
  return owner;
}

const digest = (s: string) => createHash("sha256").update(s).digest();

export async function startRouter(
  options: { databaseURL?: string; quiet?: boolean } = {},
) {
  let { state } = await loadOrCreateState();
  const store = new PostgresStore(
    options.databaseURL ?? process.env.DATABASE_URL ?? LOCAL_DATABASE_URL,
  );
  await store.migrate();
  const control = new ControlPlane(store);
  try {
    await provision(control, store, state);
  } catch (e) {
    // The owner key from bootstrap expires after 30 days. Start a fresh local workspace.
    if (!(e instanceof RouterError) || e.status !== 401) throw e;
    ({ state } = await loadOrCreateState(true));
    await provision(control, store, state);
    if (!options.quiet)
      console.log(
        "[router] owner key expired; created workspace " + state.workspace,
      );
  }

  const router = new Router(
    store,
    {
      primary: new ScenarioProvider("primary"),
      backup: new ScenarioProvider("backup"),
    },
    state.digest_key,
  );
  const api = createRouterServer(router, store, control, "self-host");
  const handle = api.listeners("request")[0] as (
    req: IncomingMessage,
    res: ServerResponse,
  ) => void;
  const ownerDigest = digest(state.owner_key);

  // Local-only switch for the mock providers' behaviour, guarded by the owner key.
  const server = createServer(async (req, res) => {
    const path = new URL(req.url ?? "/", "http://localhost").pathname;
    if (path !== "/__dev/scenario") return handle(req, res);
    const token = (req.headers.authorization ?? "").replace(/^Bearer /, "");
    if (!timingSafeEqual(digest(token), ownerDigest)) {
      res.writeHead(401).end();
      return;
    }
    if (req.method === "POST") {
      let raw = "";
      for await (const chunk of req) raw += chunk;
      let next: unknown;
      try {
        next = JSON.parse(raw).scenario;
      } catch {
        next = undefined;
      }
      if (!isScenario(next)) {
        res.writeHead(400, { "content-type": "application/json" });
        res.end(JSON.stringify({ error: { code: "invalid_scenario" } }));
        return;
      }
      scenario.current = next;
    }
    res.writeHead(200, { "content-type": "application/json" });
    res.end(
      JSON.stringify({ scenario: scenario.current, scenarios: SCENARIOS }),
    );
  });
  server.listen(PORTS.router, HOST);
  await once(server, "listening");
  if (!options.quiet)
    console.log(
      `[router]  http://${HOST}:${PORTS.router}  workspace ${state.workspace}`,
    );
  return {
    server,
    store,
    state,
    async close() {
      await new Promise<void>((r) => server.close(() => r()));
      await store.close();
    },
  };
}
