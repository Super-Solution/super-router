import { OpenAIProvider } from "@superrouter/provider-openai";
import { AnthropicProvider } from "@superrouter/provider-anthropic";
import { demoInput, demoService } from "./demo-config.js";
// Explicit command, no hidden paid calls in test/demo. No credential values or output text printed.
const chosen = process.env.SMOKE_PROVIDER;
if (chosen !== "openai" && chosen !== "anthropic")
  throw new Error(
    "Set SMOKE_PROVIDER=openai or anthropic. This explicitly invokes the selected paid provider.",
  );
const apiKey =
  chosen === "openai"
    ? process.env.OPENAI_API_KEY
    : process.env.ANTHROPIC_API_KEY;
const model =
  chosen === "openai" ? process.env.OPENAI_MODEL : process.env.ANTHROPIC_MODEL;
if (!apiKey || !model)
  throw new Error("Selected provider key/model missing; no request sent.");
const provider =
  chosen === "openai"
    ? new OpenAIProvider(() => apiKey)
    : new AnthropicProvider(() => apiKey);
const offering = { ...demoService.offerings[0]!, upstream_model: model };
let textChars = 0,
  usage: unknown,
  done = false;
for await (const e of provider.invoke(
  { ...demoInput, limits: { max_output_tokens: 64, timeout_ms: 30000 } },
  offering,
  AbortSignal.timeout(30000),
)) {
  if (e.type === "delta") textChars += e.text.length;
  if (e.type === "usage") usage = e.usage;
  if (e.type === "done") done = true;
}
if (!done || !usage) throw new Error("No confirmed terminal usage.");
console.log(
  JSON.stringify({
    provider: chosen,
    model,
    textChars,
    usage,
    done,
    mode: "live",
    recorded_at: new Date().toISOString(),
  }),
);
