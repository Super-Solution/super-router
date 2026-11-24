//! The [`Provider`] for Google's Gemini `generateContent` API. It streams text and tool calls, normalises
//! `usageMetadata` into router meters and classifies failures: a definitive rejection (the provider certainly did no
//! work) lets the router try the next offering, anything uncertain keeps the reservation pending.
//!
//! The same wire format is served by the Gemini API (API key header, `/v1beta`) and by Vertex AI (OAuth bearer
//! token, a project and location in the path). The two differ only in where the request goes and how it is
//! authorised, so one provider covers both: see [`GeminiBuilder::path_prefix`] and [`GeminiBuilder::bearer_auth`].
//!
//! Redirects are never followed, API keys never appear in errors or `Debug` output, and response bodies are never
//! echoed. A completed response whose usage is missing or malformed is refused rather than billed as free.

mod tools;
mod usage;

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use async_stream::try_stream;
use futures_util::StreamExt;
use reqwest::header::{HeaderName, HeaderValue, AUTHORIZATION};
use reqwest::{redirect, Client, ClientBuilder};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use super_router_contracts::{
    classify_failure, Invocation, Offering, Provider, ProviderCapabilities, ProviderError, ProviderEvent, ProviderStream,
};

pub use usage::normalize;
pub use usage::UsageMetadata;

const DEFAULT_BASE: &str = "https://generativelanguage.googleapis.com";
const DEFAULT_PREFIX: &str = "/v1beta";

/// Supplies the credential on every call, so it can rotate. An empty credential is a definitive failure.
pub type KeySource = Arc<dyn Fn() -> String + Send + Sync>;

fn perr(definitive: bool, retryable: bool, code: &str) -> ProviderError {
    ProviderError { definitive, retryable, code: code.to_string() }
}

#[derive(Clone, Copy)]
enum Auth {
    /// `x-goog-api-key: <key>`, the Gemini API.
    ApiKey,
    /// `Authorization: Bearer <token>`, Vertex AI.
    Bearer,
}

/// Calls the Gemini `generateContent` API.
#[derive(Clone)]
pub struct Gemini {
    key: KeySource,
    client: Client,
    base: String,
    prefix: String,
    auth: Auth,
}

impl fmt::Debug for Gemini {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Gemini").field("base", &self.base).field("prefix", &self.prefix).finish_non_exhaustive()
    }
}

/// Configures a [`Gemini`] provider.
pub struct GeminiBuilder {
    key: KeySource,
    base: String,
    prefix: String,
    auth: Auth,
    client: ClientBuilder,
}

impl GeminiBuilder {
    /// Points the provider at another server (a proxy, a regional Vertex endpoint, a test server).
    #[must_use]
    pub fn base_url(mut self, url: impl Into<String>) -> Self {
        self.base = url.into();
        self
    }

    /// What goes between the host and `/models/<model>:streamGenerateContent`. The default is `/v1beta`; Vertex AI
    /// uses `/v1/projects/<project>/locations/<location>/publishers/google`.
    #[must_use]
    pub fn path_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefix = prefix.into();
        self
    }

    /// Sends the credential as `Authorization: Bearer ...` (Vertex AI access tokens) instead of `x-goog-api-key`.
    #[must_use]
    pub fn bearer_auth(mut self) -> Self {
        self.auth = Auth::Bearer;
        self
    }

    /// Starts from your own HTTP client settings (proxies, tracing, TLS roots). Redirects are always refused,
    /// whatever policy the builder carries.
    #[must_use]
    pub fn client_builder(mut self, builder: ClientBuilder) -> Self {
        self.client = builder;
        self
    }

    pub fn build(self) -> Result<Gemini, reqwest::Error> {
        let client = self.client.redirect(redirect::Policy::none()).build()?;
        Ok(Gemini {
            key: self.key,
            client,
            base: self.base.trim_end_matches('/').to_string(),
            prefix: self.prefix.trim_end_matches('/').to_string(),
            auth: self.auth,
        })
    }
}

impl Gemini {
    /// A provider for the public Gemini API.
    ///
    /// # Panics
    /// When the HTTP client cannot be built (no TLS backend). Use [`Gemini::builder`] to handle that.
    pub fn new(key: impl Into<String>) -> Self {
        Self::builder(key).build().expect("building the HTTP client")
    }

    pub fn builder(key: impl Into<String>) -> GeminiBuilder {
        let key = key.into();
        Self::builder_with_key_source(Arc::new(move || key.clone()))
    }

    /// Like [`Gemini::builder`], reading the credential on every call.
    pub fn builder_with_key_source(key: KeySource) -> GeminiBuilder {
        GeminiBuilder {
            key,
            base: DEFAULT_BASE.to_string(),
            prefix: DEFAULT_PREFIX.to_string(),
            auth: Auth::ApiKey,
            client: Client::builder(),
        }
    }
}

/// The model name as it goes into the path: without a `models/` prefix, and only characters that cannot change the
/// path or the query. Anything else would send the call somewhere the offering did not name.
fn model_segment(upstream_model: &str) -> Option<&str> {
    let name = upstream_model.strip_prefix("models/").unwrap_or(upstream_model);
    let ok = !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'));
    ok.then_some(name)
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Chunk {
    #[serde(default)]
    candidates: Option<Vec<Candidate>>,
    #[serde(default)]
    usage_metadata: Option<UsageMetadata>,
    #[serde(default)]
    response_id: Option<String>,
    #[serde(default)]
    prompt_feedback: Option<PromptFeedback>,
    #[serde(default)]
    error: Option<Value>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct PromptFeedback {
    #[serde(default)]
    block_reason: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Candidate {
    #[serde(default)]
    content: Option<Content>,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize, Default)]
struct Content {
    #[serde(default)]
    parts: Option<Vec<Part>>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Part {
    #[serde(default)]
    text: Option<String>,
    /// A thought summary, not an answer. It is never forwarded; its tokens are in `thoughtsTokenCount`.
    #[serde(default)]
    thought: Option<bool>,
    #[serde(default)]
    function_call: Option<FunctionCall>,
    /// Gemini 3 attaches an opaque signature of its reasoning to a function call and requires it back when the call
    /// is replayed in the next request. It is passed on untouched as the call's `signature`.
    #[serde(default)]
    thought_signature: Option<String>,
}

#[derive(Deserialize, Default)]
struct FunctionCall {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    args: Option<Value>,
}

fn parse_chunk(data: &str) -> Result<Chunk, ProviderError> {
    let invalid = || perr(false, false, "invalid_upstream_event");
    let value: Value = serde_json::from_str(data).map_err(|_| invalid())?;
    if !value.is_object() {
        return Err(invalid());
    }
    serde_json::from_value(value).map_err(|_| invalid())
}

/// Finish reasons (and `PROMPT_BLOCKED`, which this adapter assigns to a blocked prompt) that mean the content
/// policy ended the call, as opposed to the output limit or a malformed call.
fn refused_by_policy(finish: &str) -> bool {
    matches!(
        finish,
        "SAFETY"
            | "RECITATION"
            | "BLOCKLIST"
            | "PROHIBITED_CONTENT"
            | "SPII"
            | "IMAGE_SAFETY"
            | "IMAGE_PROHIBITED_CONTENT"
            | "PROMPT_BLOCKED"
    )
}

/// A signature only validates on the backend that minted it: the Gemini API answers a replayed Vertex signature with
/// `400 Corrupted thought signature`, and Vertex answers the Gemini API's with `400 Invalid thought signature`.
/// Telling that apart from any other 400 lets the caller see why a conversation cannot move to this offering, and
/// since another offering (the one that minted it) may accept it, the call falls through to the next one.
fn names_rejected_signature(message: &str) -> bool {
    let m = message.to_lowercase().replace('_', " ");
    m.contains("thought signature") || m.contains("thoughtsignature")
}

/// What a refusal means. Only the error's own message is read, from the first 16 KiB of a client error's body;
/// nothing of it is passed on. Server errors are not read: the class cannot change what they mean.
async fn refusal(response: &mut reqwest::Response) -> ProviderError {
    const LIMIT: usize = 16_384;
    let status = response.status().as_u16();
    let mut message = String::new();
    if (400..500).contains(&status) {
        let mut body = Vec::new();
        while body.len() < LIMIT {
            match response.chunk().await {
                Ok(Some(chunk)) => body.extend_from_slice(&chunk),
                _ => break,
            }
        }
        body.truncate(LIMIT);
        message = serde_json::from_slice::<Value>(&body)
            .ok()
            .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
            .unwrap_or_default();
    }
    if status == 400 && names_rejected_signature(&message) {
        return perr(true, true, "thought_signature_rejected");
    }
    classify_failure(status, &message).provider_error(status)
}

impl Provider for Gemini {
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities { tools: true, images: true, structured_output: true }
    }

    fn invoke(&self, input: &Invocation, offering: &Offering) -> ProviderStream {
        let this = self.clone();
        let is_task = input.input.task.is_some();
        let system: Vec<&str> = input.input.messages.iter().filter(|m| m.role == "system").map(|m| m.content.as_str()).collect();
        let system = system.join("\n");
        let contents = tools::contents(&input.input.messages);
        let mut body = Map::new();
        tools::tool_fields(&mut body, &input.input);
        let max_output = input.limits.max_output_tokens;
        // The schema goes in as JSON Schema; the API has no place for its name or description, and a schema is always
        // enforced (the contract refuses `strict: false`).
        let response_schema = input.response_format.as_ref().map(|f| Value::Object(f.json_schema.schema.clone()));
        let timeout = u64::try_from(input.limits.timeout_ms).ok().filter(|ms| *ms > 0).map(Duration::from_millis);
        let model = model_segment(&offering.upstream_model).map(str::to_string);
        Box::pin(try_stream! {
            if is_task {
                Err(perr(true, false, "unsupported_task"))?;
            }
            let key = (this.key)();
            if key.is_empty() {
                Err(perr(true, false, "credentials_missing"))?;
            }
            let model = model.ok_or_else(|| perr(true, false, "request_invalid"))?;
            let contents = contents.map_err(|_| perr(true, false, "request_invalid"))?;
            if contents.is_empty() {
                Err(perr(true, false, "messages_required"))?;
            }
            // A credential that cannot be a header value never leaves the process, so nothing was dispatched.
            let (name, value) = match this.auth {
                Auth::ApiKey => (HeaderName::from_static("x-goog-api-key"), HeaderValue::from_str(&key)),
                Auth::Bearer => (AUTHORIZATION, HeaderValue::from_str(&format!("Bearer {key}"))),
            };
            let mut credential = value.map_err(|_| perr(true, false, "request_invalid"))?;
            credential.set_sensitive(true);
            body.insert("contents".into(), Value::Array(contents));
            if !system.is_empty() {
                body.insert("systemInstruction".into(), json!({ "parts": [{ "text": system }] }));
            }
            let mut config = json!({ "maxOutputTokens": max_output });
            if let Some(schema) = response_schema {
                config["responseMimeType"] = json!("application/json");
                config["responseJsonSchema"] = schema;
            }
            body.insert("generationConfig".into(), config);
            let url = format!("{}{}/models/{model}:streamGenerateContent?alt=sse", this.base, this.prefix);
            let mut request = this.client.post(url).header(name, credential).json(&body);
            if let Some(timeout) = timeout {
                request = request.timeout(timeout);
            }
            let mut response = request.send().await.map_err(|_| perr(false, false, "transport_unknown"))?;
            let status = response.status().as_u16();
            if !(200..=299).contains(&status) {
                Err(refusal(&mut response).await)?;
            }
            let mut frames = Box::pin(super_router_sse::frames(response.bytes_stream()));
            let mut upstream_id = String::new();
            // Every chunk repeats the running totals, so the last one seen is the call's usage.
            let mut usage: Option<UsageMetadata> = None;
            let mut finish: Option<String> = None;
            let mut calls = 0_usize;
            let bad = || perr(false, false, "invalid_upstream_event");
            while let Some(item) = frames.next().await {
                let frame = item.map_err(|e| match e {
                    super_router_sse::Error::FrameTooLarge => perr(false, false, "stream_frame_too_large"),
                    super_router_sse::Error::Read(_) => perr(false, false, "stream_interrupted"),
                })?;
                let chunk = parse_chunk(&frame.data)?;
                if chunk.error.is_some() {
                    Err(perr(false, false, "upstream_incomplete"))?;
                }
                if upstream_id.is_empty() {
                    upstream_id = chunk.response_id.clone().unwrap_or_default();
                }
                if chunk.usage_metadata.is_some() {
                    usage = chunk.usage_metadata;
                }
                if let Some(candidate) = chunk.candidates.and_then(|c| c.into_iter().next()) {
                    for part in candidate.content.and_then(|c| c.parts).unwrap_or_default() {
                        if let Some(call) = part.function_call {
                            // A call without its name cannot be answered later: refuse it rather than invent one.
                            let name = call.name.filter(|n| !n.is_empty()).ok_or_else(bad)?;
                            let args = match call.args {
                                None | Some(Value::Null) => json!({}),
                                Some(v @ Value::Object(_)) => v,
                                Some(_) => Err(bad())?,
                            };
                            let id = call.id.filter(|i| !i.is_empty()).unwrap_or_else(|| {
                                let stem = if upstream_id.is_empty() { "call" } else { upstream_id.as_str() };
                                format!("{stem}_{calls}")
                            });
                            let arguments_delta = serde_json::to_string(&args).map_err(|_| bad())?;
                            let signature = part.thought_signature.filter(|s| !s.is_empty());
                            yield ProviderEvent::ToolCall { index: calls, id: Some(id), name: Some(name), signature, arguments_delta };
                            calls += 1;
                        } else if part.thought != Some(true) {
                            if let Some(text) = part.text.filter(|t| !t.is_empty()) {
                                yield ProviderEvent::Delta { text };
                            }
                        }
                    }
                    if let Some(reason) = candidate.finish_reason.filter(|r| !r.is_empty()) {
                        finish = Some(reason);
                    }
                }
                // A blocked prompt has no candidate and no finish reason; it still ends the call.
                if finish.is_none() && chunk.prompt_feedback.and_then(|f| f.block_reason).is_some() {
                    finish = Some("PROMPT_BLOCKED".to_string());
                }
            }
            // The server closes the stream after its last chunk, which is the one that carries the final usage.
            let finish = finish.ok_or_else(|| perr(false, false, "stream_interrupted"))?;
            let usage = usage.ok_or_else(|| perr(false, false, "usage_unknown"))?;
            let meters = normalize(&usage).ok_or_else(|| perr(false, false, "invalid_usage"))?;
            yield ProviderEvent::Usage { usage: meters, upstream_id };
            // Anything but a natural stop cut the answer short: by the output limit, or by the content policy.
            let completion = match finish.as_str() {
                "STOP" => "succeeded",
                f if refused_by_policy(f) => "filtered",
                _ => "partial",
            };
            yield ProviderEvent::Done { completion: Some(completion.to_string()) };
        })
    }
}
