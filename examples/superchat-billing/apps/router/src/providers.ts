// Two local model providers that never call a paid upstream. A shared scenario decides
// how they behave, so the console can show what the ledger does when things go wrong.
import {
  ProviderError,
  type Invocation,
  type Offering,
  type Provider,
} from "@supersolutions/super-router-contracts";
import type { ScenarioId } from "@supersolutions/example-superchat-shared";
import { randomUUID } from "node:crypto";

export const scenario: { current: ScenarioId } = { current: "normal" };

const sleep = (ms: number, signal: AbortSignal) =>
  new Promise<void>((resolve, reject) => {
    const t = setTimeout(resolve, ms);
    signal.addEventListener(
      "abort",
      () => {
        clearTimeout(t);
        reject(new ProviderError(false, false, "cancelled"));
      },
      { once: true },
    );
  });

/** Rough token estimate: about four characters per token. */
const tokens = (text: string) => Math.max(1, Math.ceil(text.length / 4));

function compose(input: Invocation, offering: Offering) {
  const last =
    [...input.input.messages].reverse().find((m) => m.role === "user")
      ?.content ?? "";
  const quoted = last.length > 140 ? last.slice(0, 137) + "…" : last;
  return [
    `You asked: “${quoted}”.`,
    `This answer comes from ${offering.id}, a local mock model, so no real provider was paid.`,
    `SuperRouter still reserved credits before the call and will settle on the tokens below.`,
    `Open the console to see this request, its attempts and what was charged.`,
  ].join(" ");
}

export class ScenarioProvider implements Provider {
  constructor(private role: "primary" | "backup") {}

  async *invoke(input: Invocation, offering: Offering, signal: AbortSignal) {
    const s = scenario.current;
    await sleep(200, signal);
    if (this.role === "primary") {
      // Refused before doing any work: the router may try the next candidate.
      if (s === "fallback" || s === "all-fail")
        throw new ProviderError(true, true, "upstream_unavailable");
      // Sent, then no answer: it might have run, so it must not be retried elsewhere.
      if (s === "unknown") {
        await sleep(600, signal);
        throw new ProviderError(false, false, "upstream_timeout");
      }
    }
    if (this.role === "backup" && s === "all-fail")
      throw new ProviderError(true, true, "upstream_unavailable");

    const reply = compose(input, offering);
    const words = reply.split(" ");
    const cut = this.role === "primary" && s === "stream-cut";
    const stop = cut ? Math.floor(words.length / 2) : words.length;
    for (let i = 0; i < stop; i++) {
      yield { type: "delta" as const, text: (i ? " " : "") + words[i] };
      await sleep(25, signal);
    }
    // Output was delivered but final usage never arrived.
    if (cut) throw new ProviderError(false, false, "stream_interrupted");

    const prompt = input.input.messages.map((m) => m.content).join("\n");
    yield {
      type: "usage" as const,
      usage: {
        input: tokens(prompt),
        cached: 0,
        cache_write: 0,
        output: Math.min(tokens(reply), input.limits.max_output_tokens),
      },
      upstream_id: "mock-" + randomUUID(),
    };
    yield { type: "done" as const };
  }
}
