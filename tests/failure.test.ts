import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import {
  classifyFailure,
  failureError,
  isOfferingScoped,
  parseRetryAfter,
  ProviderError,
} from "@supersolutions/super-router-contracts";
import { OpenAIProvider } from "@supersolutions/super-router-provider-openai";
import { AnthropicProvider } from "@supersolutions/super-router-provider-anthropic";
import { demoInput, demoService } from "../scripts/demo-config.js";

/** conformance/failure.v1.json and retry_after.v1.json: shared by every backend (the health book is Rust and Go only). */
const read = (name: string) =>
  JSON.parse(
    readFileSync(new URL(`../conformance/${name}`, import.meta.url), "utf8"),
  );
const failure = read("failure.v1.json");
const retryAfter = read("retry_after.v1.json");
const offering = demoService.offerings[0]!;

test("a refusal's status and message classify as the shared cases say", () => {
  assert.ok(failure.cases.length >= 40);
  for (const c of failure.cases) {
    const cls = classifyFailure(c.status, c.message);
    const e = failureError(cls, c.status);
    assert.deepEqual(
      [cls, e.definitive, e.retryable, e.code, isOfferingScoped(cls)],
      [
        c.expect.class,
        c.expect.definitive,
        c.expect.retryable,
        c.expect.code,
        c.expect.offering_scoped,
      ],
      c.name,
    );
  }
});

test("a Retry-After header reads as the shared cases say", () => {
  assert.ok(retryAfter.cases.length >= 30);
  for (const c of retryAfter.cases)
    assert.equal(
      parseRetryAfter(c.header) ?? null,
      c.expect_ms,
      JSON.stringify(c.header),
    );
});

for (const [name, make] of [
  ["OpenAI", (f: typeof fetch) => new OpenAIProvider(() => "k", f)],
  ["Anthropic", (f: typeof fetch) => new AnthropicProvider(() => "k", f)],
] as const) {
  test(`${name} turns every shared provider response into the shared error`, async () => {
    assert.ok(failure.provider_responses.length >= 15);
    for (const c of failure.provider_responses) {
      const transport: typeof fetch = async () =>
        new Response(c.body, {
          status: c.status,
          headers:
            c.retry_after === null ? {} : { "retry-after": c.retry_after },
        });
      let error: unknown;
      try {
        await Array.fromAsync(
          make(transport).invoke(
            demoInput,
            offering,
            new AbortController().signal,
          ),
        );
      } catch (e) {
        error = e;
      }
      assert.ok(error instanceof ProviderError, c.name);
      assert.deepEqual(
        [
          error.definitive,
          error.retryable,
          error.code,
          error.retry_after_ms ?? null,
        ],
        [
          c.expect.definitive,
          c.expect.retryable,
          c.expect.code,
          c.expect.retry_after_ms,
        ],
        c.name,
      );
    }
  });
}
