import { paymentIntegrations } from "./payments.js";
import { Router, MockProvider } from "@superrouter/core";
import {
  PostgresStore,
  ControlPlane,
  LOCAL_DATABASE_URL,
} from "@superrouter/storage-postgres";
import { RouterError } from "@superrouter/contracts";
import { createRouterServer } from "./server.js";
const apiKey = process.env.SUPERROUTER_API_KEY,
  digestKey = process.env.SUPERROUTER_DIGEST_KEY;
if (
  !apiKey ||
  !/^sr_[a-f0-9]{64}$/.test(apiKey) ||
  !digestKey ||
  digestKey.length < 32
)
  throw new Error(
    "Set SUPERROUTER_API_KEY (sr_ plus 64 random hexadecimal characters) and SUPERROUTER_DIGEST_KEY (>=32 chars). Values are not logged.",
  );
const store = new PostgresStore(process.env.DATABASE_URL ?? LOCAL_DATABASE_URL);
try {
  await store.migrate();
  const secret = process.env.SUPERROUTER_SANDBOX_PAYMENT_SECRET;
  const control = new ControlPlane(store, [
    ...paymentIntegrations("demo", "test"),
    ...(secret
      ? [
          {
            id: "local-sandbox",
            workspace: "demo",
            environment: "test",
            secret,
            mode: "sandbox" as const,
          },
        ]
      : []),
  ]);
  try {
    await control.bootstrap("demo", "test", "owner", apiKey);
  } catch (e) {
    if (!(e instanceof RouterError && e.code === "already_bootstrapped"))
      throw e;
    const p = await control.authenticate(apiKey);
    if (
      p.workspace !== "demo" ||
      p.environment !== "test" ||
      !p.scopes.includes("identity:write")
    )
      throw new Error("Existing bootstrap key does not authorize demo/test.");
  }
  const server = createRouterServer(
    new Router(store, { mock: new MockProvider() }, digestKey),
    store,
    control,
  );
  server.listen(Number(process.env.PORT ?? 3410), "127.0.0.1", () =>
    console.log(
      "SuperRouter local MOCK server; persistent database authorization; listening on 127.0.0.1:" +
        String(process.env.PORT ?? 3410),
    ),
  );
  for (const event of ["SIGINT", "SIGTERM"] as const)
    process.once(event, () => {
      server.close(() => {
        void store.close();
      });
    });
} catch (e) {
  await store.close();
  throw e;
}
