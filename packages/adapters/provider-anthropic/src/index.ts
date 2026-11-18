import {
  Meters,
  ProviderError,
  parseSSE,
  type Provider,
  type Invocation,
  type Offering,
} from "@supersolutions/super-router-contracts";
export function normalizeAnthropic(raw: Record<string, any>) {
  return Meters.parse({
    input: raw.input_tokens,
    cached: raw.cache_read_input_tokens ?? 0,
    cache_write: raw.cache_creation_input_tokens ?? 0,
    output: raw.output_tokens,
  });
}
/** The Messages API `messages` list. Plain text messages pass through unchanged. */
export function toAnthropicMessages(messages: Invocation["input"]["messages"]) {
  const out: { role: string; content: unknown }[] = [];
  let results: unknown[] | undefined;
  for (const m of messages) {
    if (m.role === "system") continue;
    if (m.role === "tool") {
      const block = {
        type: "tool_result",
        tool_use_id: m.tool_call_id,
        content: m.content,
        ...(m.is_error ? { is_error: true } : {}),
      };
      // Results of one step travel together in a single user message.
      if (results) results.push(block);
      else out.push({ role: "user", content: (results = [block]) });
      continue;
    }
    results = undefined;
    if ("tool_calls" in m)
      out.push({
        role: "assistant",
        content: [
          ...(m.content ? [{ type: "text", text: m.content }] : []),
          ...m.tool_calls.map((call) => ({
            type: "tool_use",
            id: call.id,
            name: call.name,
            input: JSON.parse(call.arguments),
          })),
        ],
      });
    else if (typeof m.content === "string") out.push(m);
    else
      out.push({
        role: m.role,
        content: m.content.map((part) => {
          if (part.type === "text") return part;
          const data = /^data:([^;]+);base64,(.*)$/s.exec(part.url);
          return {
            type: "image",
            source: data
              ? { type: "base64", media_type: data[1], data: data[2] }
              : { type: "url", url: part.url },
          };
        }),
      });
  }
  return out;
}
function toolFields(input: Invocation["input"]) {
  const choice = input.tool_choice;
  return {
    ...(input.tools
      ? {
          tools: input.tools.map((tool) => ({
            name: tool.name,
            ...(tool.description ? { description: tool.description } : {}),
            input_schema: tool.input_schema,
          })),
        }
      : {}),
    ...(choice === undefined
      ? {}
      : {
          tool_choice:
            typeof choice === "object"
              ? { type: "tool", name: choice.name }
              : { type: choice === "required" ? "any" : choice },
        }),
  };
}
export class AnthropicProvider implements Provider {
  capabilities = { tools: true, images: true };
  constructor(
    private key: () => string,
    private transport: typeof fetch = fetch,
  ) {}
  async *invoke(input: Invocation, offering: Offering, signal: AbortSignal) {
    if (input.input.task)
      throw new ProviderError(true, false, "unsupported_task");
    const key = this.key();
    if (!key) throw new ProviderError(true, false, "credentials_missing");
    const messages = toAnthropicMessages(input.input.messages);
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
          ...toolFields(input.input),
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
    // Upstream numbers every content block; tool calls are numbered from zero among themselves.
    const calls = new Map<
      number,
      { index: number; streamed: boolean; input: unknown }
    >();
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
      if (
        e.type === "content_block_start" &&
        e.content_block?.type === "tool_use"
      ) {
        const index = calls.size;
        calls.set(e.index, {
          index,
          streamed: false,
          input: e.content_block.input,
        });
        yield {
          type: "tool_call" as const,
          index,
          id: String(e.content_block.id),
          name: String(e.content_block.name),
          arguments_delta: "",
        };
      }
      if (
        e.type === "content_block_delta" &&
        e.delta?.type === "input_json_delta"
      ) {
        const call = calls.get(e.index);
        if (!call)
          throw new ProviderError(false, false, "invalid_upstream_event");
        call.streamed = true;
        yield {
          type: "tool_call" as const,
          index: call.index,
          arguments_delta: String(e.delta.partial_json),
        };
      }
      if (e.type === "content_block_stop") {
        const call = calls.get(e.index);
        // A block may carry its whole input at the start and stream nothing.
        if (
          call &&
          !call.streamed &&
          call.input &&
          Object.keys(call.input).length
        )
          yield {
            type: "tool_call" as const,
            index: call.index,
            arguments_delta: JSON.stringify(call.input),
          };
      }
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
