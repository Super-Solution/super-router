// Walks through what the ledger does in each situation, against the running router.
// Start the example first (npm run demo:superchat), then: npm run scenarios:superchat
import { randomUUID } from "node:crypto";
import {
  APP_MEMBER,
  APP_SCOPES,
  CREDIT_ASSET,
  ROUTER_URL,
  SCENARIOS,
  SERVICE_ID,
  formatCredits,
  type ScenarioId,
} from "@supersolutions/example-superchat-shared";
import { readState } from "@supersolutions/example-superchat-shared/state";

const state = await readState();
if (!state)
  throw new Error(
    "No local state. Start the example first: npm run demo:superchat",
  );

async function call(
  key: string,
  path: string,
  init: { method?: string; body?: unknown; idem?: string } = {},
) {
  const r = await fetch(ROUTER_URL + path, {
    method: init.method ?? "GET",
    redirect: "manual",
    headers: {
      authorization: "Bearer " + key,
      "content-type": "application/json",
      ...(init.idem ? { "idempotency-key": init.idem } : {}),
    },
    ...(init.body === undefined ? {} : { body: JSON.stringify(init.body) }),
  });
  const text = await r.text();
  return {
    status: r.status,
    location: r.headers.get("location"),
    data: text ? JSON.parse(text) : null,
  };
}

const owner = state.owner_key;
const keyFor = async (customer: string) => {
  const r = await call(owner, "/v1/admin/keys", {
    method: "POST",
    idem: randomUUID(),
    body: {
      member_id: APP_MEMBER,
      customer_id: customer,
      scopes: APP_SCOPES,
      service_ids: [SERVICE_ID],
      expires_at: new Date(Date.now() + 3_600_000).toISOString(),
    },
  });
  if (r.status !== 201)
    throw new Error("could not issue key: " + JSON.stringify(r.data));
  return r.data.token as string;
};
const balance = async (key: string) => {
  const r = await call(key, "/v1/credits");
  return (r.data.buckets as { asset: string; available: string }[])
    .filter((b) => b.asset === CREDIT_ASSET)
    .reduce((s, b) => s + BigInt(b.available), 0n);
};
const setScenario = (scenario: ScenarioId) =>
  call(owner, "/__dev/scenario", { method: "POST", body: { scenario } });
const ask = (
  key: string,
  customer: string,
  idem: string,
  text = "Explain the bill for this message.",
) =>
  call(key, "/v1/inferences", {
    method: "POST",
    idem,
    body: {
      service: SERVICE_ID,
      customer_id: customer,
      input: { messages: [{ role: "user", content: text }] },
      limits: { max_output_tokens: 256, timeout_ms: 30000 },
    },
  });
const lastAttempts = async () => {
  const r = await call(owner, "/v1/admin/requests?limit=1");
  const row = r.data.data[0];
  return row.attempts
    .map(
      (a: { offering: string; status: string }) => `${a.offering}:${a.status}`,
    )
    .join(" → ");
};

const alice = await keyFor("alice");
const rows: string[][] = [];
const record = async (
  name: string,
  before: bigint,
  result: { status: number; data: any },
) => {
  const after = await balance(alice);
  const billing = result.data?.billing_status ?? result.data?.error?.code ?? "";
  // A pending request keeps its reservation: the credits are held, not charged.
  const change = formatCredits(before - after, 6);
  rows.push([
    name,
    String(result.status),
    billing,
    await lastAttempts(),
    billing === "pending" ? "held " + change : change,
  ]);
};

try {
  for (const s of SCENARIOS) {
    await setScenario(s.id);
    const before = await balance(alice);
    await record(
      s.label,
      before,
      await ask(alice, "alice", "scenario-" + s.id + "-" + randomUUID()),
    );
  }
  await setScenario("normal");

  // The same Idempotency-Key and body: the router answers with the original request.
  const idem = "retry-" + randomUUID();
  let before = await balance(alice);
  await ask(alice, "alice", idem);
  const replay = await ask(alice, "alice", idem);
  rows.push([
    "Retry, same key",
    `${replay.status} → ${replay.location}`,
    "replayed",
    "(no new attempt)",
    formatCredits(before - (await balance(alice)), 6) + " total",
  ]);

  // Same key, different body: rejected rather than silently answered.
  const conflict = await ask(alice, "alice", idem, "A different question");
  rows.push([
    "Same key, new body",
    String(conflict.status),
    conflict.data?.error?.code ?? "",
    "",
    "0",
  ]);

  // Bob starts with 0.12 credits. Each request first holds its worst case (~0.097), so he runs out quickly.
  const bob = await keyFor("bob");
  let last = { status: 0, data: null as any };
  for (let i = 0; i < 20 && last.status !== 402; i++)
    last = await ask(bob, "bob", "bob-" + randomUUID());
  rows.push([
    "Bob runs out",
    String(last.status),
    last.data?.error?.code ?? "",
    "",
    "left " + formatCredits(await balance(bob), 6),
  ]);

  // A revoked key stops working immediately.
  const temp = await keyFor("alice");
  const me = await call(temp, "/v1/me");
  await call(owner, `/v1/admin/keys/${me.data.key_id}/revoke`, {
    method: "POST",
    idem: randomUUID(),
  });
  const revoked = await ask(temp, "alice", "revoked-" + randomUUID());
  rows.push([
    "Revoked key",
    String(revoked.status),
    revoked.data?.error?.code ?? "",
    "",
    "0",
  ]);
} finally {
  await setScenario("normal");
}

const head = [
  "Situation",
  "HTTP",
  "Billing / error",
  "Attempts",
  "Balance change (credits)",
];
const widths = head.map((h, i) =>
  Math.max(h.length, ...rows.map((r) => r[i]!.length)),
);
const line = (r: string[]) => r.map((c, i) => c.padEnd(widths[i]!)).join("  ");
console.log(
  "\n" + line(head) + "\n" + widths.map((w) => "-".repeat(w)).join("  "),
);
for (const r of rows) console.log(line(r));
console.log(
  "\nEvery row is also in the console's request log: http://127.0.0.1:4420\n",
);
