import test from "node:test";
import assert from "node:assert/strict";
import { once } from "node:events";
import { randomUUID } from "node:crypto";
import {
  compileCreditPrice,
  publishCreditRateVersion,
  charge,
  Router,
  MockProvider,
  type CreditRateConfig,
} from "@superrouter/core";
import {
  PostgresStore,
  ControlPlane,
  LOCAL_DATABASE_URL,
} from "@superrouter/storage-postgres";
import { Service, type Principal, type Provider } from "@superrouter/contracts";
import {
  demoService,
  demoInput,
  digestKey,
  principal,
  seed,
} from "../scripts/demo-config.js";
import { SuperRouterClient } from "@superrouter/sdk";
import { createRouterServer } from "../apps/server/server.js";
const config: CreditRateConfig = {
  asset: "credit:demo",
  tokens: {
    input: { units: "1000" },
    cached: { units: "2000" },
    cache_write: { units: "500" },
    output: { units: "250" },
  },
};
test("credit ratios compile exact tokens/cache/media without changing atom scale", () => {
  const price = compileCreditPrice({
    ...config,
    media: {
      images: { units: "5" },
      video_ms: { units: "20000" },
      audio_ms: { units: "60000" },
      tasks: { units: "2" },
    },
  });
  assert.equal(price.input, "1000000");
  assert.equal(price.per, 1);
  assert.equal(price.media!.images!.atoms, "200000000");
  assert.equal(price.media!.video_ms!.atoms, "50000");
});
test("exact rational rates reject hidden precision loss, floats, zero and unknown fields", () => {
  const p = compileCreditPrice({
    asset: "credit:demo",
    tokens: {
      input: { units: "3" },
      cached: { units: "7" },
      cache_write: { units: "1" },
      output: { units: "1000", credits: "2" },
    },
  });
  assert.equal(p.per, 21);
  assert.equal(p.input, "7000000000");
  assert.equal(p.cached, "3000000000");
  assert.equal(
    charge({ input: 3, cached: 7, cache_write: 0, output: 0 }, p),
    2000000000n,
  );
  assert.equal(
    compileCreditPrice({
      asset: "credit:demo",
      media: { audio_ms: { units: "60000" } },
    }).media!.audio_ms!.per,
    3,
  );
  for (const bad of [
    { ...config, scale: 6 },
    { asset: "credit:demo" },
    { asset: "credit:demo", tokens: { input: { units: "1000" } } },
    { ...config, tokens: { ...config.tokens, input: { units: "1000000007" } } },
    { ...config, tokens: { ...config.tokens, input: { units: "0" } } },
    { ...config, tokens: { ...config.tokens, input: { units: 1.5 } } },
  ])
    assert.throws(() => compileCreditPrice(bad));
  assert.throws(
    () =>
      compileCreditPrice({
        asset: "credit:demo",
        tokens: {
          ...config.tokens,
          input: { units: "1", credits: "999999999999999999999999999999" },
        },
      }),
    /unsupported_rate_precision/,
  );
});
async function fixture() {
  const store = new PostgresStore(
    process.env.DATABASE_URL ?? LOCAL_DATABASE_URL,
  );
  await store.migrate();
  const p = { ...principal, workspace: "rates-" + randomUUID() };
  await seed(store, p, demoService, "1000000000000");
  return { store, p };
}
test("published rates preserve outstanding request/replay snapshots and take effect for new requests", async () => {
  const { store, p } = await fixture();
  let entered!: () => void, release!: () => void;
  const ready = new Promise<void>((r) => (entered = r)),
    gate = new Promise<void>((r) => (release = r));
  const base = new MockProvider();
  const provider: Provider = {
    async *invoke(...args) {
      entered();
      await gate;
      yield* base.invoke(...args);
    },
  };
  const router = new Router(store, { mock: provider }, digestKey);
  try {
    const first = router.infer(p, demoInput, "old");
    await ready;
    const next = await publishCreditRateVersion(store, p, "chat", 1, config);
    assert.equal(next.version, 2);
    assert.deepEqual(next.offerings, demoService.offerings);
    assert.deepEqual(next.platform_price, demoService.platform_price);
    release();
    const old = await first;
    assert.equal(old.billing!.charges[0]!.atoms, "35");
    assert.equal(old.service_version, 1);
    const newer = await router.infer(p, demoInput, "new");
    assert.equal(newer.service_version, 2);
    assert.equal(newer.billing!.charges[0]!.atoms, "30000000");
    await assert.rejects(
      () =>
        router.infer(p, { ...demoInput, service_version: 1 }, "new-pinned-old"),
      /inactive_service_version/,
    );
    const replay = await router.infer(p, demoInput, "old");
    assert.equal(replay.id, old.id);
    assert.equal(base.calls, 2);
    await assert.rejects(
      () => publishCreditRateVersion(store, p, "chat", 1, config),
      /service_version_conflict/,
    );
    await assert.rejects(
      () =>
        publishCreditRateVersion(store, p, "chat", 2, {
          ...config,
          asset: "credit:other",
        }),
      /credit_asset_mismatch/,
    );
    await assert.rejects(
      () =>
        store.publish(p, {
          ...demoService,
          version: 1,
          price: { ...demoService.price, input: "9" },
        }),
      /version_exists/,
    );
    await assert.rejects(
      () => store.publish(p, { ...demoService, version: 0 }),
      /Too small/,
    );
  } finally {
    release();
    await store.close();
  }
});
test("concurrent expected-version edits allow only one new version; gaps cannot roll back active service", async () => {
  const { store, p } = await fixture();
  try {
    const edits = await Promise.allSettled([
      publishCreditRateVersion(store, p, "chat", 1, config),
      publishCreditRateVersion(store, p, "chat", 1, {
        ...config,
        tokens: { ...config.tokens, input: { units: "2000" } },
      }),
    ]);
    assert.equal(edits.filter((e) => e.status === "fulfilled").length, 1);
    await store.publish(p, {
      ...(await store.getService(p, "chat")),
      version: 4,
    });
    await assert.rejects(
      () => store.publish(p, { ...demoService, version: 3 }),
      /service_version_conflict/,
    );
    assert.equal((await store.getService(p, "chat")).version, 4);
  } finally {
    await store.close();
  }
});
async function cloudFixture() {
  const store = new PostgresStore(
    process.env.DATABASE_URL ?? LOCAL_DATABASE_URL,
  );
  await store.migrate();
  const control = new ControlPlane(store);
  const workspace = "cloud-rates-" + randomUUID(),
    bootstrap = await control.bootstrap(workspace, "test"),
    owner = await control.authenticate(bootstrap.token);
  await control.member(
    owner,
    { id: "developer", role: "service_admin" },
    "member",
  );
  const key = await control.issueKey(
    owner,
    {
      member_id: "developer",
      customer_id: "_admin",
      scopes: ["service:write"],
      expires_at: new Date(Date.now() + 86400000).toISOString(),
    },
    "key",
  );
  const developer = await control.authenticate(key.token!);
  const trusted: Principal = { ...owner, auth_key_id: undefined };
  const service = Service.parse({
    ...demoService,
    platform_price: { ...demoService.price, asset: "credit:platform" },
    offerings: demoService.offerings.map((o) => ({ ...o, source: "platform" })),
  });
  return {
    store,
    control,
    owner,
    developer,
    trusted,
    service,
    key: key.token!,
  };
}
test("cloud developer rate endpoint protects platform supply, auth, ownership and optimistic version", async () => {
  const f = await cloudFixture();
  const server = createRouterServer(
    new Router(f.store, {}, digestKey),
    f.store,
    f.control,
    "cloud",
  );
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  const addr = server.address();
  assert.ok(addr && typeof addr === "object");
  const url = `http://127.0.0.1:${addr.port}/v1/admin/services/chat/credit-rates`;
  try {
    await assert.rejects(
      () => f.store.publish(f.developer, f.service, "cloud"),
      /platform_pricing_protected/,
    );
    await f.store.publish(f.trusted, f.service, "cloud");
    for (const bad of [
      { ...f.service, version: 2, platform_price: undefined },
      {
        ...f.service,
        version: 2,
        platform_price: { ...f.service.platform_price!, input: "0" },
      },
      {
        ...f.service,
        version: 2,
        offerings: f.service.offerings.map((o) => ({
          ...o,
          price: { ...o.price, input: "0" },
        })),
      },
      {
        ...f.service,
        version: 2,
        offerings: f.service.offerings.map((o) => ({ ...o, id: "other" })),
      },
    ])
      await assert.rejects(
        () => f.store.publish(f.developer, bad, "cloud"),
        /platform_pricing_protected/,
      );
    await assert.rejects(
      () =>
        f.store.publish(
          f.developer,
          { ...f.service, version: 2, offerings: demoService.offerings },
          "cloud",
        ),
      /byok_not_allowed/,
    );
    const request = (token: string, body: unknown) =>
      fetch(url, {
        method: "POST",
        headers: {
          authorization: "Bearer " + token,
          "content-type": "application/json",
          "idempotency-key": randomUUID(),
        },
        body: JSON.stringify(body),
      });
    assert.equal(
      (await request("", { expected_version: 1, config })).status,
      401,
    );
    const sdk = new SuperRouterClient(url.split("/v1/")[0]!, f.key);
    const next = await sdk.publishCreditRateVersion(
      "chat",
      1,
      config,
      randomUUID(),
    );
    assert.deepEqual(next.platform_price, f.service.platform_price);
    assert.deepEqual(next.offerings, f.service.offerings);
    assert.equal(
      (await request(f.key, { expected_version: 1, config })).status,
      409,
    );
    const forbidden = await fetch(url.replace("/chat/", "/other/"), {
      method: "POST",
      headers: {
        authorization: "Bearer " + f.key,
        "content-type": "application/json",
        "idempotency-key": randomUUID(),
      },
      body: JSON.stringify({ expected_version: 1, config }),
    });
    assert.equal(forbidden.status, 404);
    await f.store.publish(
      f.developer,
      { ...next, version: 3, offerings: demoService.offerings },
      "self-host",
    );
    assert.equal((await f.store.getService(f.developer, "chat")).version, 3);
  } finally {
    await new Promise<void>((r) => server.close(() => r()));
    await f.store.close();
  }
});

test("pricing edits must match service meters and never publish a hidden free rate", async () => {
  const { store, p } = await fixture();
  try {
    await assert.rejects(
      () =>
        publishCreditRateVersion(store, p, "chat", 1, {
          asset: "credit:demo",
          media: { images: { units: "5" } },
        }),
      /credit_rate_kind_mismatch/,
    );
    assert.equal((await store.getService(p, "chat")).version, 1);
    const video = Service.parse({
      ...demoService,
      id: "video",
      task_kind: "video",
      offerings: demoService.offerings.map((o) => ({
        ...o,
        task_kind: "video",
        max_usage: { video_ms: 60000 },
      })),
    });
    await store.publish(p, video);
    await assert.rejects(
      () =>
        publishCreditRateVersion(store, p, "video", 1, {
          asset: "credit:demo",
          media: { audio_ms: { units: "1000" } },
        }),
      /credit_rate_kind_mismatch/,
    );
    assert.equal((await store.getService(p, "video")).version, 1);
    const next = await publishCreditRateVersion(store, p, "video", 1, {
      asset: "credit:demo",
      media: { video_ms: { units: "20000" } },
    });
    assert.equal(next.version, 2);
    await assert.rejects(
      () =>
        publishCreditRateVersion(
          store,
          p,
          "chat",
          Number.MAX_SAFE_INTEGER,
          config,
        ),
      /invalid_service_version/,
    );
  } finally {
    await store.close();
  }
});

test("rate publication between service read and reservation rejects stale admission atomically", async () => {
  const { store, p } = await fixture();
  const provider = new MockProvider();
  const router = new Router(store, { mock: provider }, digestKey);
  const originalAccounts = store.accounts.bind(store);
  let reached!: () => void, resume!: () => void;
  const atAccounts = new Promise<void>((resolve) => (reached = resolve));
  const gate = new Promise<void>((resolve) => (resume = resolve));
  store.accounts = async (...args) => {
    const legs = await originalAccounts(...args);
    reached();
    await gate;
    return legs;
  };
  try {
    const before = await store.balance(p);
    const pending = router.infer(p, demoInput, "raced-v1");
    const rejected = assert.rejects(
      () => pending,
      (error: unknown) => {
        assert.ok(
          error instanceof Error && "code" in error && "status" in error,
        );
        assert.equal(error.code, "inactive_service_version");
        assert.equal(error.status, 409);
        return true;
      },
    );
    await atAccounts;
    await publishCreditRateVersion(store, p, "chat", 1, config);
    resume();
    await rejected;
    assert.equal(provider.calls, 0);
    assert.deepEqual(await store.balance(p), before);
    const rows = await store.pool.query(
      "SELECT count(*)::int AS count FROM sr_requests WHERE workspace=$1 AND environment=$2",
      [p.workspace, p.environment],
    );
    assert.equal(rows.rows[0].count, 0);
    store.accounts = originalAccounts;
    const next = await router.infer(p, demoInput, "fresh-v2");
    assert.equal(next.service_version, 2);
    assert.equal(next.billing_status, "settled");
    assert.equal(next.billing!.charges[0]!.atoms, "30000000");
    assert.equal(provider.calls, 1);
  } finally {
    resume();
    store.accounts = originalAccounts;
    await store.close();
  }
});
