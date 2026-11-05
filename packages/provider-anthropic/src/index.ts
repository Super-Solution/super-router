import {
  Meters,
  ProviderError,
  parseSSE,
  type Provider,
  type Invocation,
  type Offering,
} from "@superrouter/contracts";
export function normalizeAnthropic(raw: Record<string, any>) {
  return Meters.parse({
    input: raw.input_tokens,
    cached: raw.cache_read_input_tokens ?? 0,
    cache_write: raw.cache_creation_input_tokens ?? 0,
    output: raw.output_tokens,
  });
}
export class AnthropicProvider implements Provider {
  constructor(
    private key: () => string,
    private transport: typeof fetch = fetch,
  ) {}
  async *invoke(input: Invocation, offering: Offering, signal: AbortSignal) {
    if (input.input.task)
      throw new ProviderError(true, false, "unsupported_task");
    const key = this.key();
    if (!key) throw new ProviderError(true, false, "credentials_missing");
    const messages = input.input.messages.filter((m) => m.role !== "system");
    if (!messages.length)
      throw new ProviderError(true, false, "messages_required");
    let response: Response;
    try {
      response = await this.transport("https://api.anthropic.com/v1/messages", {
        method: "POST",
        redirect: "error",
        signal,
        headers: {
          "x-api-key": key,
          "anthropic-version": "2023-06-01",
          "Content-Type": "application/json",
        },
        body: JSON.stringify({
          model: offering.upstream_model,
          max_tokens: input.limits.max_output_tokens,
          system: input.input.messages
            .filter((m) => m.role === "system")
            .map((m) => m.content)
            .join("\n"),
          messages,
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
    let usage: Record<string, any> | null = null,
      id: string | undefined,
      terminal = false,
      completion: "succeeded" | "partial" = "succeeded";
    for await (const frame of parseSSE(response.body)) {
      let e: any;
      try {
        e = JSON.parse(frame.data);
      } catch {
        throw new ProviderError(false, false, "invalid_upstream_event");
      }
      if (e.type === "message_start") {
        usage = e.message?.usage ?? null;
        id = e.message?.id;
      }
      if (e.type === "content_block_delta" && e.delta?.type === "text_delta")
        yield { type: "delta" as const, text: String(e.delta.text) };
      if (e.type === "message_delta" && usage) {
        usage = { ...usage, ...e.usage };
        if (e.delta?.stop_reason === "max_tokens") completion = "partial";
      }
      if (e.type === "message_stop") {
        if (!usage) throw new ProviderError(false, false, "usage_unknown");
        yield {
          type: "usage" as const,
          usage: normalizeAnthropic(usage),
          upstream_id: id,
        };
        terminal = true;
        yield { type: "done" as const, completion };
        break;
      }
      if (e.type === "error")
        throw new ProviderError(false, false, "upstream_stream_error");
    }
    if (!terminal) throw new ProviderError(false, false, "stream_interrupted");
  }
}
