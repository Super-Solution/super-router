import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import {
  Invocation,
  Service,
  EventSchema,
  z,
} from "@supersolutions/super-router-contracts";
import { demoInput, demoService } from "../scripts/demo-config.js";
test("generated API schemas stay synchronized with strict runtime contracts", async () => {
  const api = JSON.parse(await readFile("docs/generated/openapi.json", "utf8"));
  assert.deepEqual(
    api.components.schemas.Invocation,
    z.toJSONSchema(Invocation, { target: "draft-2020-12" }),
  );
  assert.deepEqual(
    api.components.schemas.Service,
    z.toJSONSchema(Service, { target: "draft-2020-12" }),
  );
  assert.ok(Invocation.safeParse(demoInput).success);
  assert.ok(Service.safeParse(demoService).success);
  assert.equal(
    Invocation.safeParse({ ...demoInput, payer_mode: "user" }).success,
    false,
  );
  assert.equal(
    EventSchema.safeParse({
      id: "evt_1",
      type: "billing.settled",
      schema_version: 1,
      workspace_id: "ws",
      environment_id: "test",
      request_id: "req",
      occurred_at: "2026-11-04T15:00:00Z",
    }).success,
    true,
  );
  assert.equal(
    Service.safeParse({
      ...demoService,
      weights: { cost: 0, latency: 0, quality: 0, reliability: 0 },
    }).success,
    false,
  );
});
