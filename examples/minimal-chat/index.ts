import { randomUUID } from "node:crypto";
import { once } from "node:events";
import { Router, MockProvider } from "@supersolutions/super-router-core";
import { SuperRouterClient } from "@supersolutions/super-router-sdk";
import {
  PostgresStore,
  LOCAL_DATABASE_URL,
} from "@supersolutions/super-router-storage-postgres";
import { createRouterServer } from "@supersolutions/super-router-server";
import {
  principal,
  seed,
  digestKey,
  demoInput,
} from "../../scripts/demo-config.js";
const store = new PostgresStore(process.env.DATABASE_URL ?? LOCAL_DATABASE_URL);
await store.migrate();
const p = { ...principal, workspace: "demo-" + randomUUID() };
await seed(store, p);
const provider = new MockProvider();
const router = new Router(store, { mock: provider }, digestKey);
const key = randomUUID() + randomUUID();
const server = createRouterServer(router, store, [{ key, principal: p }]);
try {
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("listen failed");
  const client = new SuperRouterClient(`http://127.0.0.1:${address.port}`, key);
  const idem = randomUUID();
  let id = "";
  console.log(
    "MOCK provider; REAL PostgreSQL and HTTP/SSE. No paid upstream calls.",
  );
  for await (const event of client.stream(demoInput, idem)) {
    id = event.request_id;
    console.log(JSON.stringify(event));
  }
  console.log("status", await client.getRequest(id));
  // Same body/key replays state rather than calling upstream again.
  for await (const event of client.stream(demoInput, idem))
    console.log("replay", event);
  console.log("balance", await store.balance(p));
  console.log("provider calls", provider.calls);
  console.log("trace", JSON.stringify(await store.trace(p, id), null, 2));
} finally {
  await new Promise<void>((resolve, reject) =>
    server.close((e) => (e ? reject(e) : resolve())),
  );
  await store.close();
}
