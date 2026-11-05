import test from "node:test";
import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import {
  Invocation,
  Service,
  type Provider,
  type Usage,
} from "@superrouter/contracts";
import { charge, MockTaskProvider, Router } from "@superrouter/core";
import {
  PostgresStore,
  LOCAL_DATABASE_URL,
} from "@superrouter/storage-postgres";
import {
  demoService,
  principal,
  digestKey,
  seed,
} from "../scripts/demo-config.js";
const tokenZero = {
  input: "0",
  cached: "0",
  cache_write: "0",
  output: "0",
  per: 1,
};
const zero = { input: 0, cached: 0, cache_write: 0, output: 0 };
function config(kind: "image" | "video" | "audio" | "task" = "video") {
  const key = {
    image: "images",
    video: "video_ms",
    audio: "audio_ms",
    task: "tasks",
  }[kind];
  const media = {
    [key]: { atoms: "3", per: kind === "video" || kind === "audio" ? 1000 : 1 },
  };
  return Service.parse({
    ...demoService,
    id: `task-${kind}`,
    task_kind: kind,
    price: { ...tokenZero, asset: "credit:demo", media },
    offerings: [
      {
        ...demoService.offerings[0],
        provider: "mock-task",
        task_kind: kind,
        max_usage: { [key]: kind === "task" ? 1 : 60000 },
        price: { ...tokenZero, asset: "USD", media },
      },
    ],
  });
}
function input(
  kind: "image" | "video" | "audio" | "task" = "video",
  units = 1500,
) {
  const key = {
    image: "images",
    video: "video_ms",
    audio: "audio_ms",
    task: "tasks",
  }[kind];
  return Invocation.parse({
    service: `task-${kind}`,
    customer_id: principal.customer,
    input: { task: { kind, prompt: "Simulate" } },
    limits: { max_usage: { [key]: units } },
  });
}
async function run(provider: Provider, raw = input(), service = config()) {
  const store = new PostgresStore(
    process.env.DATABASE_URL ?? LOCAL_DATABASE_URL,
  );
  await store.migrate();
  const p = { ...principal, workspace: `meters-${randomUUID()}` };
  await seed(store, p, service);
  const router = new Router(store, { "mock-task": provider }, digestKey);
  try {
    return await router.infer(p, raw, randomUUID());
  } finally {
    await store.close();
  }
}
test("independent integer media rates round each meter once without floating point", () => {
  assert.equal(
    charge(
      { ...zero, images: 2, video_ms: 1501, audio_ms: 1, tasks: 1 },
      {
        ...tokenZero,
        asset: "USD",
        media: {
          images: { atoms: "7", per: 1 },
          video_ms: { atoms: "3", per: 1000 },
          audio_ms: { atoms: "1", per: 1000 },
          tasks: { atoms: "9007199254740993", per: 1 },
        },
      },
    ),
    9007199254741013n,
  );
  assert.throws(
    () => charge({ ...zero, images: 1 }, { ...tokenZero, asset: "USD" }),
    /unpriced_meter/,
  );
  for (const value of [-1, 1.5, Number.MAX_SAFE_INTEGER + 1])
    assert.equal(
      Invocation.safeParse({
        ...input(),
        limits: { max_usage: { video_ms: value } },
      }).success,
      false,
    );
  assert.equal(
    Invocation.safeParse({
      ...input(),
      input: { task: { kind: "video", prompt: "x" }, usage: { video_ms: 1 } },
    }).success,
    false,
  );
});
test("each supported task settles trusted usage and exact receipt", async () => {
  for (const kind of ["image", "video", "audio", "task"] as const) {
    const result = await run(
      new MockTaskProvider(),
      input(kind, kind === "task" ? 1 : kind === "image" ? 2 : 1500),
      config(kind),
    );
    assert.equal(result.billing_status, "settled");
    assert.equal(result.execution_status, "succeeded");
    assert.match(result.output!.content, /Simulated.*no media generated/);
    assert.equal(
      result.billing!.charges[0]!.atoms,
      kind === "task" ? "3" : kind === "image" ? "6" : "5",
    );
  }
});
test("partial duration settles completed units; partial task never bills success", async () => {
  const partial = await run(new MockTaskProvider("partial"));
  assert.equal(partial.execution_status, "partial");
  assert.equal(partial.billing!.charges[0]!.atoms, "3");
  const task = await run(
    new MockTaskProvider("partial"),
    input("task", 1),
    config("task"),
  );
  assert.equal(task.usage!.meters?.tasks, 0);
  assert.equal(task.billing!.charges[0]!.atoms, "0");
});
test("definitive failure releases, unknown retains reservation", async () => {
  assert.equal(
    (await run(new MockTaskProvider("reject"))).billing_status,
    "released",
  );
  assert.equal(
    (await run(new MockTaskProvider("unknown"))).billing_status,
    "pending",
  );
});
test("task cap, kind, money bounds and invalid meter reject before dispatch", async () => {
  const provider = new MockTaskProvider();
  await assert.rejects(
    () => run(provider, input("video", 60001)),
    /no_eligible_offering/,
  );
  await assert.rejects(
    () => run(provider, { ...input("audio"), service: "task-video" }, config()),
    /no_eligible_offering/,
  );
  await assert.rejects(
    () =>
      run(provider, {
        ...input(),
        limits: {
          ...input().limits,
          max_charge: { asset: "credit:demo", atoms: "4", scale: 9 },
        },
      }),
    /budget_exceeded/,
  );
  await assert.rejects(
    () => run(provider, input("task", 2), config("task")),
    /invalid_task_limits/,
  );
  assert.equal(provider.calls, 0);
});
test("provider cap violations, incompatible meters, missing terminal usage retain hold", async () => {
  for (const usage of [
    { ...zero, video_ms: 1501 },
    { ...zero, images: 1 },
    null,
  ] as (Usage | null)[]) {
    const provider: Provider = {
      async *invoke() {
        if (usage) yield { type: "usage", usage };
        yield { type: "done" };
      },
    };
    const result = await run(provider);
    assert.equal(result.billing_status, "pending");
    assert.equal(result.billing!.charges.length, 0);
  }
});
test("zero-priced over-cap usage remains unknown across crash and worker recovery", async () => {
  const store = new PostgresStore(
    process.env.DATABASE_URL ?? LOCAL_DATABASE_URL,
  );
  await store.migrate();
  const p = { ...principal, workspace: `meters-crash-${randomUUID()}` };
  const service = config();
  service.price.media!.video_ms!.atoms = "0";
  service.offerings[0]!.price.media!.video_ms!.atoms = "0";
  await seed(store, p, service);
  const provider: Provider = {
    async *invoke() {
      yield { type: "usage", usage: { ...zero, video_ms: 1501 } };
      yield { type: "done" };
    },
  };
  const router = new Router(store, { "mock-task": provider }, digestKey);
  const finish = store.finish.bind(store);
  store.finish = async () => {
    throw new Error("simulated crash before finish");
  };
  try {
    await assert.rejects(
      () => router.infer(p, input(), "crash"),
      /execution_unknown/,
    );
    const rows = await store.pool.query(
      "SELECT id FROM sr_requests WHERE workspace=$1",
      [p.workspace],
    );
    const id = rows.rows[0].id;
    const trace = await store.trace(p, id);
    assert.equal(trace.attempts[0].status, "unknown");
    assert.equal(trace.attempts[0].usage, null);
    store.finish = finish;
    await store.pool.query(
      "UPDATE sr_requests SET deadline=clock_timestamp()-interval '1 second' WHERE workspace=$1",
      [p.workspace],
    );
    await store.recover();
    assert.equal((await store.get(p, id)).billing, "pending");
  } finally {
    await store.close();
  }
});
