//! The [`Provider`] for the Anthropic Messages API. It streams text, normalises usage into router meters and
//! classifies failures like the OpenAI provider: redirects are never followed, keys never appear in errors, and
//! a completed message whose usage is missing or malformed is refused rather than billed as free.

mod tools;

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use async_stream::try_stream;
use futures_util::StreamExt;
use reqwest::header::{HeaderName, HeaderValue, CONTENT_TYPE};
use reqwest::{redirect, Client, ClientBuilder};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::value::RawValue;
use serde_json::{json, Map, Value};
use super_router_contracts::{
    refusal as contracts_refusal, Invocation, Meters, Offering, Provider, ProviderCapabilities, ProviderError, ProviderEvent,
    ProviderStream, REFUSAL_BODY_LIMIT,
};

const DEFAULT_BASE: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2023-06-01";

/// Supplies the API key on every call, so it can rotate. An empty key is a definitive failure.
pub type KeySource = Arc<dyn Fn() -> String + Send + Sync>;

fn perr(definitive: bool, retryable: bool, code: &str) -> ProviderError {
    ProviderError { definitive, retryable, code: code.to_string(), retry_after_ms: None }
}

/// Calls the Anthropic Messages API.
#[derive(Clone)]
pub struct Anthropic {
    key: KeySource,
    client: Client,
    base: String,
}

impl fmt::Debug for Anthropic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Anthropic").field("base", &self.base).finish_non_exhaustive()
    }
}

/// Configures an [`Anthropic`] provider.
pub struct AnthropicBuilder {
    key: KeySource,
    base: String,
    client: ClientBuilder,
}

impl AnthropicBuilder {
    /// Points the provider at an Anthropic-compatible server.
    #[must_use]
    pub fn base_url(mut self, url: impl Into<String>) -> Self {
        self.base = url.into();
        self
    }

    /// Starts from your own HTTP client settings (proxies, tracing, TLS roots). Redirects are always refused,
    /// whatever policy the builder carries.
    #[must_use]
    pub fn client_builder(mut self, builder: ClientBuilder) -> Self {
        self.client = builder;
        self
    }

    pub fn build(self) -> Result<Anthropic, reqwest::Error> {
        let client = self.client.redirect(redirect::Policy::none()).build()?;
        Ok(Anthropic { key: self.key, client, base: self.base.trim_end_matches('/').to_string() })
    }
}

impl Anthropic {
    /// A provider for the public Anthropic API.
    ///
    /// # Panics
    /// When the HTTP client cannot be built (no TLS backend). Use [`Anthropic::builder`] to handle that.
    pub fn new(key: impl Into<String>) -> Self {
        Self::builder(key).build().expect("building the HTTP client")
    }

    pub fn builder(key: impl Into<String>) -> AnthropicBuilder {
        let key = key.into();
        Self::builder_with_key_source(Arc::new(move || key.clone()))
    }

    /// Like [`Anthropic::builder`], reading the key on every call.
    pub fn builder_with_key_source(key: KeySource) -> AnthropicBuilder {
        AnthropicBuilder { key, base: DEFAULT_BASE.to_string(), client: Client::builder() }
    }
}

/// Null reads as the default, the way a missing field does.
fn lenient<'de, D: Deserializer<'de>, T: Deserialize<'de> + Default>(d: D) -> Result<T, D::Error> {
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

/// The two required counters stay optional: a message that omits them must be refused, not billed as zero tokens
/// (the TypeScript adapter does the same).
#[derive(Deserialize, Default, Clone)]
struct Usage {
    #[serde(default)]
    input_tokens: Option<i64>,
    #[serde(default)]
    output_tokens: Option<i64>,
    #[serde(default, deserialize_with = "lenient")]
    cache_read_input_tokens: i64,
    #[serde(default, deserialize_with = "lenient")]
    cache_creation_input_tokens: i64,
}

#[derive(Deserialize, Default)]
struct Message {
    #[serde(default, deserialize_with = "lenient")]
    id: String,
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Deserialize, Default)]
struct Delta {
    #[serde(default, rename = "type", deserialize_with = "lenient")]
    kind: String,
    #[serde(default, deserialize_with = "lenient")]
    text: String,
    #[serde(default, deserialize_with = "lenient")]
    stop_reason: String,
    #[serde(default)]
    partial_json: Option<Value>,
}

#[derive(Deserialize, Default)]
struct Block {
    #[serde(default, rename = "type")]
    kind: Option<Value>,
    #[serde(default)]
    id: Option<Value>,
    #[serde(default)]
    name: Option<Value>,
    #[serde(default)]
    input: Option<Box<RawValue>>,
}

/// One tool call of the message: its number among the calls, whether its input was streamed, and the input its
/// block started with.
struct Call {
    index: usize,
    streamed: bool,
    input: Option<Box<RawValue>>,
}

#[derive(Deserialize, Default)]
struct Event {
    #[serde(default, rename = "type", deserialize_with = "lenient")]
    kind: String,
    #[serde(default)]
    message: Option<Message>,
    #[serde(default)]
    delta: Option<Delta>,
    #[serde(default)]
    usage: Option<Map<String, Value>>,
    #[serde(default)]
    index: Option<Value>,
    #[serde(default)]
    content_block: Option<Block>,
}

/// Read straight from the text, so a tool call's input keeps its own key order.
fn parse_event(data: &str) -> Result<Event, ProviderError> {
    let invalid = || perr(false, false, "invalid_upstream_event");
    let raw: &RawValue = serde_json::from_str(data).map_err(|_| invalid())?;
    if !raw.get().starts_with('{') {
        return Err(invalid());
    }
    serde_json::from_str(raw.get()).map_err(|_| invalid())
}

impl Usage {
    /// Anthropic's `input_tokens` counts uncached input only; the cache meters are separate.
    fn meters(&self) -> Option<Meters> {
        let meters = Meters {
            input: self.input_tokens?,
            cached: self.cache_read_input_tokens,
            cache_write: self.cache_creation_input_tokens,
            output: self.output_tokens?,
            ..Meters::default()
        };
        meters.is_valid().then_some(meters)
    }
}

/// `message_delta` carries the final counters; they overwrite what `message_start` reported. A counter that is
/// not an integer (including null) refuses the whole response.
fn apply_delta_usage(current: &mut Usage, fields: &Map<String, Value>) -> Result<(), ProviderError> {
    for (name, raw) in fields {
        let slot = match name.as_str() {
            "input_tokens" | "output_tokens" | "cache_read_input_tokens" | "cache_creation_input_tokens" => name.as_str(),
            _ => continue,
        };
        let n = raw.as_i64().ok_or_else(|| perr(false, false, "invalid_usage"))?;
        match slot {
            "input_tokens" => current.input_tokens = Some(n),
            "output_tokens" => current.output_tokens = Some(n),
            "cache_read_input_tokens" => current.cache_read_input_tokens = n,
            _ => current.cache_creation_input_tokens = n,
        }
    }
    Ok(())
}

/// The refusal an HTTP error status means: its class (from the status and the error's own message, read from the first
/// 16 KiB of a client error's body) and the wait its `Retry-After` header asked for.
async fn refusal(response: reqwest::Response) -> ProviderError {
    let status = response.status().as_u16();
    let retry_after = response.headers().get(reqwest::header::RETRY_AFTER).and_then(|v| v.to_str().ok()).map(str::to_string);
    let mut body = Vec::new();
    if (400..500).contains(&status) {
        let mut chunks = response.bytes_stream();
        while body.len() < REFUSAL_BODY_LIMIT {
            match chunks.next().await {
                Some(Ok(chunk)) => body.extend_from_slice(&chunk),
                _ => break,
            }
        }
    }
    contracts_refusal(status, retry_after.as_deref(), &body)
}

/// A request body: the plain fields, then the messages.
#[derive(Serialize)]
struct Body<'a> {
    #[serde(flatten)]
    fields: Map<String, Value>,
    messages: &'a [tools::Turn],
}

impl Provider for Anthropic {
    fn capabilities(&self) -> ProviderCapabilities {
        // The Messages API call here has no seed and no reasoning effort: a request that asks for them is refused before reserving.
        ProviderCapabilities {
            tools: true,
            images: true,
            temperature: true,
            top_p: true,
            stop: true,
            structured_output: true,
            ..ProviderCapabilities::default()
        }
    }

    fn invoke(&self, input: &Invocation, offering: &Offering) -> ProviderStream {
        let this = self.clone();
        let is_task = input.input.task.is_some();
        let system: Vec<&str> = input.input.messages.iter().filter(|m| m.role == "system").map(|m| m.content.as_str()).collect();
        let system = system.join("\n");
        let turns = tools::messages(&input.input.messages);
        let mut fields = Map::new();
        tools::tool_fields(&mut fields, &input.input);
        if let Some(s) = &input.sampling {
            if let Some(t) = s.temperature {
                fields.insert("temperature".into(), json!(t));
            }
            if let Some(p) = s.top_p {
                fields.insert("top_p".into(), json!(p));
            }
            if let Some(stop) = &s.stop {
                fields.insert("stop_sequences".into(), json!(stop));
            }
        }
        if let Some(format) = &input.response_format {
            // The Messages API has no place for the format's name, description or strict flag (a schema is always
            // enforced): only the schema is sent. A refusal or an output cut by `max_tokens` need not match it.
            fields.insert("output_config".into(), json!({"format": {"type": "json_schema", "schema": format.json_schema.schema}}));
        }
        // Automatic prompt caching (5 minutes, written at 1.25 times the input price): only when asked for. Absent and `none`
        // send nothing, so nothing is cached and no write is billed. The Anthropic API only: Amazon Bedrock refuses it.
        if input.cache.as_ref().is_some_and(|c| c.mode == "auto") {
            fields.insert("cache_control".into(), json!({"type": "ephemeral"}));
        }
        let max_tokens = input.limits.max_output_tokens;
        let timeout = u64::try_from(input.limits.timeout_ms).ok().filter(|ms| *ms > 0).map(Duration::from_millis);
        let model = offering.upstream_model.clone();
        Box::pin(try_stream! {
            if is_task {
                Err(perr(true, false, "unsupported_task"))?;
            }
            let key = (this.key)();
            if key.is_empty() {
                Err(perr(true, false, "credentials_missing"))?;
            }
            // Tool-call arguments that are not JSON, from a hand-built invocation, never leave the process.
            let turns = turns.map_err(|_| perr(true, false, "request_invalid"))?;
            if turns.is_empty() {
                Err(perr(true, false, "messages_required"))?;
            }
            // A key that cannot be a header value never leaves the process, so nothing was dispatched.
            let mut api_key = HeaderValue::from_str(&key).map_err(|_| perr(true, false, "request_invalid"))?;
            api_key.set_sensitive(true);
            fields.insert("model".into(), json!(model));
            fields.insert("max_tokens".into(), json!(max_tokens));
            fields.insert("stream".into(), json!(true));
            // Omitted rather than sent empty when there is no system message.
            if !system.is_empty() {
                fields.insert("system".into(), Value::String(system));
            }
            let body = serde_json::to_vec(&Body { fields, messages: &turns }).map_err(|_| perr(true, false, "request_invalid"))?;
            let mut request = this
                .client
                .post(format!("{}/v1/messages", this.base))
                .header(HeaderName::from_static("x-api-key"), api_key)
                .header("anthropic-version", API_VERSION)
                .header(CONTENT_TYPE, "application/json")
                .body(body);
            if let Some(timeout) = timeout {
                request = request.timeout(timeout);
            }
            let response = request.send().await.map_err(|_| perr(false, false, "transport_unknown"))?;
            let status = response.status().as_u16();
            let response = if (200..=299).contains(&status) {
                response
            } else {
                Err::<(), _>(refusal(response).await)?;
                unreachable!("a refusal always ends the stream")
            };
            let mut frames = Box::pin(super_router_sse::frames(response.bytes_stream()));
            let mut current: Option<Usage> = None;
            let mut id = String::new();
            let mut completion = "succeeded";
            let mut terminal = false;
            // Upstream numbers every content block; tool calls are numbered from zero among themselves.
            let mut calls: HashMap<i64, Call> = HashMap::new();
            let bad = || perr(false, false, "invalid_upstream_event");
            while let Some(item) = frames.next().await {
                let frame = item.map_err(|e| match e {
                    super_router_sse::Error::FrameTooLarge => perr(false, false, "stream_frame_too_large"),
                    super_router_sse::Error::Read(_) => perr(false, false, "stream_interrupted"),
                })?;
                let event = parse_event(&frame.data)?;
                match event.kind.as_str() {
                    "message_start" => {
                        let message = event.message.unwrap_or_default();
                        current = message.usage;
                        id = message.id;
                    }
                    "content_block_start" => {
                        let block = event.content_block.unwrap_or_default();
                        if block.kind.as_ref().and_then(Value::as_str) == Some("tool_use") {
                            // A call without its id, name or position cannot be answered later: refuse it rather
                            // than invent one.
                            let id = tools::text(block.id.as_ref()).ok_or_else(bad)?;
                            let name = tools::text(block.name.as_ref()).ok_or_else(bad)?;
                            let position = tools::safe_index(event.index.as_ref()).ok_or_else(bad)?;
                            if calls.contains_key(&position) {
                                Err(bad())?;
                            }
                            let index = calls.len();
                            calls.insert(position, Call { index, streamed: false, input: block.input });
                            yield ProviderEvent::ToolCall { index, id: Some(id), name: Some(name), signature: None, arguments_delta: String::new() };
                        }
                    }
                    "content_block_stop" => {
                        // A block may carry its whole input at the start and stream nothing.
                        let call = tools::safe_index(event.index.as_ref()).and_then(|p| calls.get(&p)).filter(|c| !c.streamed);
                        if let Some(call) = call {
                            if let Some(arguments) = tools::whole_input(call.input.as_deref()) {
                                yield ProviderEvent::ToolCall { index: call.index, id: None, name: None, signature: None, arguments_delta: arguments };
                            }
                        }
                    }
                    "content_block_delta" => match event.delta {
                        Some(delta) if delta.kind == "text_delta" => yield ProviderEvent::Delta { text: delta.text },
                        Some(delta) if delta.kind == "input_json_delta" => {
                            let call = tools::safe_index(event.index.as_ref()).and_then(|p| calls.get_mut(&p)).ok_or_else(bad)?;
                            let partial = tools::text(delta.partial_json.as_ref()).ok_or_else(bad)?;
                            call.streamed = true;
                            yield ProviderEvent::ToolCall { index: call.index, id: None, name: None, signature: None, arguments_delta: partial };
                        }
                        _ => {}
                    },
                    "message_delta" => {
                        if let Some(usage) = current.as_mut() {
                            if let Some(fields) = &event.usage {
                                apply_delta_usage(usage, fields)?;
                            }
                            if event.delta.is_some_and(|d| d.stop_reason == "max_tokens") {
                                completion = "partial";
                            }
                        }
                    }
                    "message_stop" => {
                        let usage = current.take().ok_or_else(|| perr(false, false, "usage_unknown"))?;
                        let meters = usage.meters().ok_or_else(|| perr(false, false, "invalid_usage"))?;
                        yield ProviderEvent::Usage { usage: meters, upstream_id: std::mem::take(&mut id) };
                        terminal = true;
                        yield ProviderEvent::Done { completion: Some(completion.to_string()) };
                        break;
                    }
                    "error" => Err(perr(false, false, "upstream_stream_error"))?,
                    _ => {}
                }
            }
            if !terminal {
                Err(perr(false, false, "stream_interrupted"))?;
            }
        })
    }
}
