import {
  Meters,
  ProviderError,
  parseSSE,
  type Provider,
  type Invocation,
  type Offering,
} from "@superrouter/contracts";
export function normalizeOpenAI(raw: Record<string, any>) {
  const input = raw.input_tokens,
    cached = raw.input_tokens_details?.cached_tokens ?? 0,
    write = raw.input_tokens_details?.cache_write_tokens ?? 0;
  return Meters.parse({
    input: input - cached - write,
    cached,
    cache_write: write,
    output: raw.output_tokens,
  });
}
export class OpenAIProvider implements Provider {
  constructor(
    private key: () => string,
    private transport: typeof fetch = fetch,
  ) {}
  async *invoke(input: Invocation, offering: Offering, signal: AbortSignal) {
    if (input.input.task)
      throw new ProviderError(true, false, "unsupported_task");
    const key = this.key();
    if (!key) throw new ProviderError(true, false, "credentials_missing");
    let response: Response;
    try {
      response = await this.transport("https://api.openai.com/v1/responses", {
        method: "POST",
        redirect: "error",
        signal,
        headers: {
          Authorization: `Bearer ${key}`,
          "Content-Type": "application/json",
        },
        body: JSON.stringify({
          model: offering.upstream_model,
          input: input.input.messages,
          max_output_tokens: input.limits.max_output_tokens,
          store: false,
          stream: true,
        }),
      });
    } catch {
      throw new ProviderError(false, false, "transport_unknown");
    }
    if (!response.ok) {
      await response.body?.cancel();
      throw new ProviderError(
        [400, 401, 403, 404, 422, 429].includes(response.status),
        response.status === 429,
        "upstream_http_" + response.status,
      );
    }
    if (!response.body) throw new ProviderError(false, false, "empty_stream");
    let terminal = false;
    for await (const frame of parseSSE(response.body)) {
      let e: any;
      try {
        e = JSON.parse(frame.data);
      } catch {
        throw new ProviderError(false, false, "invalid_upstream_event");
      }
      if (e.type === "response.output_text.delta")
        yield { type: "delta" as const, text: String(e.delta) };
      if (e.type === "response.completed" || e.type === "response.incomplete") {
        if (!e.response?.usage)
          throw new ProviderError(false, false, "usage_unknown");
        yield {
          type: "usage" as const,
          usage: normalizeOpenAI(e.response.usage),
          upstream_id: e.response.id,
        };
        terminal = true;
        yield {
          type: "done" as const,
          completion:
            e.type === "response.incomplete"
              ? ("partial" as const)
              : ("succeeded" as const),
        };
        break;
      }
      if (["error", "response.failed", "response.incomplete"].includes(e.type))
        throw new ProviderError(false, false, "upstream_incomplete");
    }
    if (!terminal) throw new ProviderError(false, false, "stream_interrupted");
  }
}
