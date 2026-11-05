import { Service, Invocation, type Principal } from "@superrouter/contracts";
import { PostgresStore } from "@superrouter/storage-postgres";
export const principal: Principal = {
  workspace: "demo",
  environment: "test",
  customer: "customer_demo",
  scopes: [
    "invoke",
    "service:write",
    "billing:write",
    "billing:read",
    "operations:read",
    "routing:write",
  ],
};
export const digestKey = "local-demo-digest-key-not-for-production";
export const demoService = Service.parse({
  id: "chat",
  version: 1,
  mode: "tier",
  tier: "chat/low",
  regions: ["test"],
  required_labels: [],
  price: {
    asset: "credit:demo",
    input: "2",
    cached: "1",
    cache_write: "2",
    output: "3",
    per: 1,
  },
  max_attempts: 2,
  max_provider_cost: "10000",
  max_output_tokens: 128,
  weights: { cost: 1, latency: 0, reliability: 0, quality: 0 },
  cost_scale: "10000",
  latency_scale_ms: 1000,
  offerings: [
    {
      id: "mock-a",
      provider: "mock",
      model: "mock-model",
      upstream_model: "mock-model",
      region: "test",
      labels: [],
      enabled: true,
      source: "byok",
      max_input_tokens: 1024,
      max_output_tokens: 128,
      price: {
        asset: "USD",
        input: "1",
        cached: "1",
        cache_write: "1",
        output: "1",
        per: 1,
      },
    },
  ],
});
export const demoInput = Invocation.parse({
  service: "chat",
  customer_id: "customer_demo",
  payer_mode: "developer",
  input: { messages: [{ role: "user", content: "Hello" }] },
  limits: { max_output_tokens: 32, timeout_ms: 30000 },
  stream: false,
});
export async function seed(
  store: PostgresStore,
  p = principal,
  s = demoService,
  amount = "100000",
) {
  try {
    await store.getService(p, s.id, s.version);
  } catch {
    await store.publish(p, s);
  }
  await store.grant(
    p,
    "customer_sale",
    s.price.asset,
    amount,
    "initial-demo-grant",
  );
  if (s.platform_price)
    await store.grant(
      p,
      "platform_sale",
      s.platform_price.asset,
      amount,
      "initial-platform-grant",
    );
}
