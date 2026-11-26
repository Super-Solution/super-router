// Writes conformance/failure.v1.json: what an upstream refusal means for the next offering.
//
// Each case is an HTTP status and the error's own message, and the expected class with what it implies: whether the
// provider certainly did no work (`definitive`), whether another offering may be tried (`retryable`) and the error
// code. There is no TypeScript reference for this classification (Rust and Go only), so this script is the
// independent oracle: it states the rule once, in plain JavaScript, with no code from either backend.
//
//   node scripts/export-failure-conformance.mjs

import { writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const file = join(dirname(fileURLToPath(import.meta.url)), "..", "conformance", "failure.v1.json");

// Messages are lower-cased and `_` / `-` read as spaces; a class matches when one of its phrases is contained.
const phrases = {
  credentials: ["api key not valid", "api key expired", "api key invalid", "invalid api key", "incorrect api key", "invalid x api key"],
  quota: ["insufficient quota", "exceeded your current quota", "credit balance is too low", "credits are depleted", "spend limit", "spending cap", "requires billing to be enabled", "billing account", "per day"],
  region: ["user location is not supported", "not available in your country", "not available in your region", "not supported in your region"],
  model: ["model not found", "is not found for api version", "does not exist or you do not have access"],
};
const has = (message, list) => {
  const m = message.toLowerCase().replace(/[_-]/g, " ");
  return list.some((p) => m.includes(p));
};

function classOf(status, message) {
  if (has(message, phrases.credentials)) return "credentials_rejected";
  if (has(message, phrases.region)) return "region_blocked";
  if (status === 401) return "credentials_rejected";
  if (status === 402) return "quota_exhausted";
  if ([400, 403, 429].includes(status) && has(message, phrases.quota)) return "quota_exhausted";
  if (status === 403) return "credentials_rejected";
  if (status === 404) return "model_unavailable";
  if (status === 429) return "rate_limited";
  if (status === 400 && has(message, phrases.model)) return "model_unavailable";
  if (status >= 400 && status < 500 && status !== 408) return "request_rejected";
  return "unknown";
}

function implies(cls, status) {
  if (["rate_limited", "quota_exhausted", "credentials_rejected", "model_unavailable", "region_blocked"].includes(cls)) {
    return { definitive: true, retryable: true, code: cls, offering_scoped: true };
  }
  if (cls === "request_rejected") return { definitive: true, retryable: false, code: `upstream_http_${status}`, offering_scoped: false };
  return { definitive: false, retryable: false, code: `upstream_http_${status}`, offering_scoped: false };
}

const cases = [];
const add = (name, status, message = "") => {
  const cls = classOf(status, message);
  cases.push({ name, status, message, expect: { class: cls, ...implies(cls, status) } });
};

for (const status of [400, 401, 402, 403, 404, 405, 408, 409, 413, 422, 429, 499, 500, 501, 502, 503, 504, 529, 200, 302, 0, 600]) add(`status ${status} alone`, status);
add("gemini key not valid", 400, "API key not valid. Please pass a valid API key.");
add("gemini key expired", 400, "API key expired. Please renew the API key.");
add("key invalid code", 400, "API_KEY_INVALID");
add("openai wrong key", 401, "Incorrect API key provided: sk-abc***");
add("anthropic wrong key header", 401, "invalid x-api-key");
add("key rejected behind a 403", 403, "The API key you provided is not valid");
add("permission denied is the key's", 403, "Permission denied on resource project 123");
add("gemini region", 400, "User location is not supported for the API use.");
add("region behind a 403", 403, "Not available in your country");
add("anthropic credit", 400, "Your credit balance is too low to access the Anthropic API");
add("openai quota", 429, "You exceeded your current quota, please check your plan and billing details.");
add("openai quota code", 429, "insufficient_quota");
add("spend cap behind a 403", 403, "Monthly spend limit reached");
add("daily quota", 429, "Quota exceeded: 1500 requests per day");
add("per minute limit", 429, "Quota exceeded for metric generate_content requests per minute");
add("plain rate limit", 429, "Rate limit reached for requests");
add("model gone", 404, "models/gemini-old is not found for API version v1beta");
add("model behind a 400", 400, "The model `gpt-9` does not exist or you do not have access to it.");
add("unrelated 400", 400, "Unknown name \"foo\" at 'tools': Cannot find field.");
add("role message", 400, "Please use a valid role: user, model.");
add("a word that is not a phrase", 400, "invalid argument: contents must not be empty");
add("too large", 413, "Request payload size exceeds the limit");
add("unprocessable", 422, "unsupported parameter");
add("a server error that names a key", 500, "internal error while validating the API key");
add("a server error that names quota", 503, "quota backend unavailable");
add("overloaded", 529, "Overloaded");
add("case and separators", 400, "Api-Key-Not-Valid");

// What the HTTP providers that read `error.message` (OpenAI Responses, Anthropic Messages) make of a refusal: the
// status, the Retry-After header and the body. Only a client error's body is read, and only `error.message` of it.
const parseMessage = (body) => {
  try {
    const m = JSON.parse(body)?.error?.message;
    return typeof m === "string" ? m : "";
  } catch {
    return "";
  }
};
const retryAfterMs = (header) => {
  const m = /^([0-9]+)(?:\.([0-9]+))?$/.exec((header ?? "").replace(/^[ \t]+|[ \t]+$/g, ""));
  if (!m) return null;
  const seconds = m[1].length > 9 ? 86400 : Number(m[1]);
  return Math.min(seconds * 1000 + Number((m[2] ?? "").slice(0, 3).padEnd(3, "0")), 86_400_000);
};
const providerResponses = [];
const respond = (name, status, body, retryAfter) => {
  const message = status >= 400 && status < 500 ? parseMessage(body) : "";
  const cls = classOf(status, message);
  providerResponses.push({
    name,
    status,
    retry_after: retryAfter ?? null,
    body,
    expect: { ...(({ definitive, retryable, code }) => ({ definitive, retryable, code }))(implies(cls, status)), retry_after_ms: retryAfterMs(retryAfter) },
  });
};
const openaiBody = (message, type = "invalid_request_error") => JSON.stringify({ error: { message, type, param: null, code: null } });
const anthropicBody = (message, type) => JSON.stringify({ type: "error", error: { type, message } });
respond("openai wrong key", 401, openaiBody("Incorrect API key provided: sk-abc***", "invalid_request_error"));
respond("anthropic wrong key", 401, anthropicBody("invalid x-api-key", "authentication_error"));
respond("anthropic permission", 403, anthropicBody("Your API key does not have permission to use the specified resource.", "permission_error"));
respond("anthropic model not found", 404, anthropicBody("model: claude-gone", "not_found_error"));
respond("openai model behind a 400", 400, openaiBody("The model `gpt-9` does not exist or you do not have access to it."));
respond("anthropic malformed request", 400, anthropicBody("messages: Input should be a valid list", "invalid_request_error"));
respond("openai rate limit with a wait", 429, openaiBody("Rate limit reached for requests", "requests"), "12");
respond("anthropic rate limit with a fractional wait", 429, anthropicBody("This request would exceed your rate limit", "rate_limit_error"), "1.5");
respond("openai empty balance behind a 429", 429, openaiBody("You exceeded your current quota, please check your plan and billing details.", "insufficient_quota"));
respond("anthropic credit balance", 400, anthropicBody("Your credit balance is too low to access the Anthropic API.", "invalid_request_error"));
respond("a rate limit whose body is not JSON", 429, "slow down", "3");
respond("a body past the limit is cut before it is read", 400, openaiBody("x".repeat(20000)));
respond("too large", 413, anthropicBody("Request exceeds the maximum allowed number of bytes.", "request_too_large"));
respond("a timeout the provider may have acted on", 408, "");
respond("server error keeps the hold and its wait", 500, openaiBody("invalid api key"), "5");
respond("overloaded keeps the hold", 529, anthropicBody("Overloaded", "overloaded_error"));
respond("unavailable with a date wait", 503, "", "Wed, 21 Oct 2026 07:28:00 GMT");

writeFileSync(file, JSON.stringify({ version: 1, description: "Upstream refusal classification (status + message -> class, definitive, retryable, code). Written by scripts/export-failure-conformance.mjs; Shared by every backend that implements it (see conformance/README.md).", cases, provider_responses: providerResponses }, null, 1) + "\n");
console.log(`failure: ${cases.length} cases, ${providerResponses.length} provider responses`);
