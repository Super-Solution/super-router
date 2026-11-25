import {
  Meters,
  ProviderError,
  parseSSE,
  refusal,
  type Provider,
  type Invocation,
  type Offering,
} from "@supersolutions/super-router-contracts";
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
/** The Responses API `input` list. Plain text messages pass through unchanged. */
export function toOpenAIInput(messages: Invocation["input"]["messages"]) {
  return messages.flatMap((m): unknown[] => {
    if (m.role === "tool")
      return [
        {
          type: "function_call_output",
          call_id: m.tool_call_id,
          output: m.content,
        },
      ];
    if ("tool_calls" in m)
      return [
        ...(m.content ? [{ role: "assistant", content: m.content }] : []),
        ...m.tool_calls.map((call) => ({
          type: "function_call",
          call_id: call.id,
          name: call.name,
          arguments: call.arguments,
        })),
      ];
    if (typeof m.content === "string") return [m];
    return [
      {
        role: m.role,
        content: m.content.map((part) =>
          part.type === "text"
            ? { type: "input_text", text: part.text }
            : {
                type: "input_image",
                image_url: part.url,
                ...(part.detail ? { detail: part.detail } : {}),
              },
        ),
      },
    ];
  });
}
function toolFields(input: Invocation["input"]) {
  const choice = input.tool_choice;
  return {
    ...(input.tools
      ? {
          tools: input.tools.map((tool) => ({
            type: "function",
            name: tool.name,
            ...(tool.description ? { description: tool.description } : {}),
            parameters: tool.input_schema,
            strict: false,
          })),
        }
      : {}),
    ...(choice === undefined
      ? {}
      : {
          tool_choice:
            typeof choice === "string"
              ? choice
              : { type: "function", name: choice.name },
        }),
  };
}
/**
 * What the Responses API takes from the request's sampling controls. It has no stop sequences and no seed, so a request
 * that asks for them is refused before reserving (`capabilities`), never sent without them.
 */
function sampling(input: Invocation) {
  return {
    ...(input.sampling?.temperature === undefined
      ? {}
      : { temperature: input.sampling.temperature }),
    ...(input.sampling?.top_p === undefined
      ? {}
      : { top_p: input.sampling.top_p }),
    ...(input.reasoning_effort === undefined
      ? {}
      : { reasoning: { effort: input.reasoning_effort } }),
  };
}
export class OpenAIProvider implements Provider {
  capabilities = {
    tools: true,
    images: true,
    temperature: true,
    top_p: true,
    reasoning_effort: true,
  };
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
          input: toOpenAIInput(input.input.messages),
          ...toolFields(input.input),
          ...sampling(input),
          max_output_tokens: input.limits.max_output_tokens,
          store: false,
          stream: true,
        }),
      });
    } catch {
      throw new ProviderError(false, false, "transport_unknown");
    }
    if (!response.ok) throw await refusal(response);
    if (!response.body) throw new ProviderError(false, false, "empty_stream");
    let terminal = false;
    // Upstream numbers every output item; tool calls are numbered from zero among themselves.
    const calls = new Map<number, { index: number; streamed: boolean }>();
    for await (const frame of parseSSE(response.body)) {
      let e: any;
      try {
        e = JSON.parse(frame.data);
      } catch {
        throw new ProviderError(false, false, "invalid_upstream_event");
      }
      if (e.type === "response.output_text.delta")
        yield { type: "delta" as const, text: String(e.delta) };
      if (
        e.type === "response.output_item.added" &&
        e.item?.type === "function_call"
      ) {
        // A call without its id, name or position cannot be answered later: refuse it rather than invent one.
        if (
          typeof e.item.call_id !== "string" ||
          typeof e.item.name !== "string" ||
          !Number.isSafeInteger(e.output_index) ||
          calls.has(e.output_index)
        )
          throw new ProviderError(false, false, "invalid_upstream_event");
        const index = calls.size;
        calls.set(e.output_index, { index, streamed: false });
        yield {
          type: "tool_call" as const,
          index,
          id: e.item.call_id,
          name: e.item.name,
          arguments_delta: "",
        };
      }
      if (e.type === "response.function_call_arguments.delta") {
        const call = calls.get(e.output_index);
        if (!call || typeof e.delta !== "string")
          throw new ProviderError(false, false, "invalid_upstream_event");
        call.streamed = true;
        yield {
          type: "tool_call" as const,
          index: call.index,
          arguments_delta: e.delta,
        };
      }
      if (e.type === "response.function_call_arguments.done") {
        const call = calls.get(e.output_index);
        if (
          !call ||
          (e.arguments !== undefined && typeof e.arguments !== "string")
        )
          throw new ProviderError(false, false, "invalid_upstream_event");
        // Some models send the arguments only here.
        if (!call.streamed)
          yield {
            type: "tool_call" as const,
            index: call.index,
            arguments_delta: e.arguments ?? "",
          };
      }
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
