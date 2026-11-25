//! The [`Provider`] for the OpenAI Responses API. It streams text, normalises usage into router meters and
//! classifies failures: a definitive rejection (the provider certainly did no work) lets the router try the next
//! offering, anything uncertain keeps the reservation pending.
//!
//! Redirects are never followed, API keys never appear in errors or `Debug` output, and response bodies are
//! never echoed. A completed response whose usage is missing, incomplete or malformed is refused rather than
//! billed as free.

mod tools;

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use async_stream::try_stream;
use futures_util::StreamExt;
use reqwest::header::{HeaderValue, AUTHORIZATION};
use reqwest::{redirect, Client, ClientBuilder};
use serde::{Deserialize, Deserializer};
use serde_json::{json, Value};
use super_router_contracts::{
    refusal as contracts_refusal, Invocation, Meters, Offering, Provider, ProviderCapabilities, ProviderError, ProviderEvent,
    ProviderStream, REFUSAL_BODY_LIMIT,
};

pub use tools::input;

const DEFAULT_BASE: &str = "https://api.openai.com";

/// Supplies the API key on every call, so it can rotate. An empty key is a definitive failure.
pub type KeySource = Arc<dyn Fn() -> String + Send + Sync>;

fn perr(definitive: bool, retryable: bool, code: &str) -> ProviderError {
    ProviderError { definitive, retryable, code: code.to_string(), retry_after_ms: None }
}

/// Calls the OpenAI Responses API.
#[derive(Clone)]
pub struct OpenAi {
    key: KeySource,
    client: Client,
    base: String,
}

impl fmt::Debug for OpenAi {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenAi").field("base", &self.base).finish_non_exhaustive()
    }
}

/// Configures an [`OpenAi`] provider.
pub struct OpenAiBuilder {
    key: KeySource,
    base: String,
    client: ClientBuilder,
}

impl OpenAiBuilder {
    /// Points the provider at an OpenAI-compatible server.
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

    pub fn build(self) -> Result<OpenAi, reqwest::Error> {
        let client = self.client.redirect(redirect::Policy::none()).build()?;
        Ok(OpenAi { key: self.key, client, base: self.base.trim_end_matches('/').to_string() })
    }
}

impl OpenAi {
    /// A provider for the public OpenAI API.
    ///
    /// # Panics
    /// When the HTTP client cannot be built (no TLS backend). Use [`OpenAi::builder`] to handle that.
    pub fn new(key: impl Into<String>) -> Self {
        Self::builder(key).build().expect("building the HTTP client")
    }

    pub fn builder(key: impl Into<String>) -> OpenAiBuilder {
        let key = key.into();
        Self::builder_with_key_source(Arc::new(move || key.clone()))
    }

    /// Like [`OpenAi::builder`], reading the key on every call.
    pub fn builder_with_key_source(key: KeySource) -> OpenAiBuilder {
        OpenAiBuilder { key, base: DEFAULT_BASE.to_string(), client: Client::builder() }
    }
}

/// Null reads as the default, the way a missing field does.
fn lenient<'de, D: Deserializer<'de>, T: Deserialize<'de> + Default>(d: D) -> Result<T, D::Error> {
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

#[derive(Deserialize, Default)]
struct Details {
    #[serde(default, deserialize_with = "lenient")]
    cached_tokens: i64,
    #[serde(default, deserialize_with = "lenient")]
    cache_write_tokens: i64,
}

/// The two required counters stay optional: a response that omits them must be refused, not billed as zero
/// tokens (the TypeScript adapter does the same).
#[derive(Deserialize, Default)]
struct Usage {
    #[serde(default)]
    input_tokens: Option<i64>,
    #[serde(default)]
    output_tokens: Option<i64>,
    #[serde(default)]
    input_tokens_details: Option<Details>,
}

impl Usage {
    /// OpenAI counts cached and cache-write tokens inside input.
    fn normalize(&self) -> Option<Meters> {
        let (input, output) = (self.input_tokens?, self.output_tokens?);
        let (cached, cache_write) = self.input_tokens_details.as_ref().map_or((0, 0), |d| (d.cached_tokens, d.cache_write_tokens));
        let meters =
            Meters { input: input.checked_sub(cached)?.checked_sub(cache_write)?, cached, cache_write, output, ..Meters::default() };
        meters.is_valid().then_some(meters)
    }
}

#[derive(Deserialize, Default)]
struct Response {
    #[serde(default, deserialize_with = "lenient")]
    id: String,
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Deserialize, Default)]
struct Event {
    #[serde(default, rename = "type", deserialize_with = "lenient")]
    kind: String,
    #[serde(default)]
    delta: Option<Value>,
    #[serde(default)]
    output_index: Option<Value>,
    #[serde(default)]
    arguments: Option<Value>,
    #[serde(default)]
    item: Option<Value>,
    #[serde(default)]
    response: Option<Response>,
}

/// One tool call of the response: its number among the calls and whether its arguments were streamed.
struct Call {
    index: usize,
    streamed: bool,
}

fn parse_event(data: &str) -> Result<Event, ProviderError> {
    let invalid = || perr(false, false, "invalid_upstream_event");
    let value: Value = serde_json::from_str(data).map_err(|_| invalid())?;
    if !value.is_object() {
        return Err(invalid());
    }
    serde_json::from_value(value).map_err(|_| invalid())
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

impl Provider for OpenAi {
    fn capabilities(&self) -> ProviderCapabilities {
        // The Responses API has no stop sequences and no seed: a request that asks for them is refused before reserving.
        ProviderCapabilities {
            tools: true,
            images: true,
            temperature: true,
            top_p: true,
            reasoning_effort: true,
            ..ProviderCapabilities::default()
        }
    }

    fn invoke(&self, input: &Invocation, offering: &Offering) -> ProviderStream {
        let this = self.clone();
        let is_task = input.input.task.is_some();
        let mut body = serde_json::Map::new();
        body.insert("input".into(), Value::Array(tools::input(&input.input.messages)));
        tools::tool_fields(&mut body, &input.input);
        if let Some(t) = input.sampling.as_ref().and_then(|s| s.temperature) {
            body.insert("temperature".into(), json!(t));
        }
        if let Some(p) = input.sampling.as_ref().and_then(|s| s.top_p) {
            body.insert("top_p".into(), json!(p));
        }
        if let Some(effort) = &input.reasoning_effort {
            body.insert("reasoning".into(), json!({ "effort": effort }));
        }
        let max_output = input.limits.max_output_tokens;
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
            // A key that cannot be a header value never leaves the process, so nothing was dispatched.
            let mut auth = HeaderValue::from_str(&format!("Bearer {key}")).map_err(|_| perr(true, false, "request_invalid"))?;
            auth.set_sensitive(true);
            body.insert("model".into(), json!(model));
            body.insert("max_output_tokens".into(), json!(max_output));
            body.insert("store".into(), json!(false));
            body.insert("stream".into(), json!(true));
            let mut request = this.client.post(format!("{}/v1/responses", this.base)).header(AUTHORIZATION, auth).json(&body);
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
            let mut terminal = false;
            // Upstream numbers every output item; tool calls are numbered from zero among themselves.
            let mut calls: HashMap<i64, Call> = HashMap::new();
            let bad = || perr(false, false, "invalid_upstream_event");
            while let Some(item) = frames.next().await {
                let frame = item.map_err(|e| match e {
                    super_router_sse::Error::FrameTooLarge => perr(false, false, "stream_frame_too_large"),
                    super_router_sse::Error::Read(_) => perr(false, false, "stream_interrupted"),
                })?;
                let event = parse_event(&frame.data)?;
                match event.kind.as_str() {
                    "response.output_text.delta" => {
                        let text = match &event.delta {
                            None | Some(Value::Null) => String::new(),
                            Some(Value::String(s)) => s.clone(),
                            Some(_) => Err(bad())?,
                        };
                        yield ProviderEvent::Delta { text };
                    }
                    "response.output_item.added" => {
                        let item = event.item.as_ref();
                        if item.and_then(|i| i.get("type")).and_then(Value::as_str) == Some("function_call") {
                            // A call without its id, name or position cannot be answered later: refuse it rather
                            // than invent one.
                            let id = tools::text(item.and_then(|i| i.get("call_id"))).ok_or_else(bad)?;
                            let name = tools::text(item.and_then(|i| i.get("name"))).ok_or_else(bad)?;
                            let position = tools::safe_index(event.output_index.as_ref()).ok_or_else(bad)?;
                            if calls.contains_key(&position) {
                                Err(bad())?;
                            }
                            let index = calls.len();
                            calls.insert(position, Call { index, streamed: false });
                            yield ProviderEvent::ToolCall { index, id: Some(id), name: Some(name), signature: None, arguments_delta: String::new() };
                        }
                    }
                    "response.function_call_arguments.delta" => {
                        let call = tools::safe_index(event.output_index.as_ref()).and_then(|p| calls.get_mut(&p)).ok_or_else(bad)?;
                        let delta = tools::text(event.delta.as_ref()).ok_or_else(bad)?;
                        call.streamed = true;
                        yield ProviderEvent::ToolCall { index: call.index, id: None, name: None, signature: None, arguments_delta: delta };
                    }
                    "response.function_call_arguments.done" => {
                        let call = tools::safe_index(event.output_index.as_ref()).and_then(|p| calls.get(&p)).ok_or_else(bad)?;
                        let arguments = match &event.arguments {
                            None => String::new(),
                            Some(Value::String(s)) => s.clone(),
                            Some(_) => Err(bad())?,
                        };
                        // Some models send the arguments only here.
                        if !call.streamed {
                            yield ProviderEvent::ToolCall { index: call.index, id: None, name: None, signature: None, arguments_delta: arguments };
                        }
                    }
                    "response.completed" | "response.incomplete" => {
                        let response = event.response.unwrap_or_default();
                        let usage = response.usage.ok_or_else(|| perr(false, false, "usage_unknown"))?;
                        let meters = usage.normalize().ok_or_else(|| perr(false, false, "invalid_usage"))?;
                        yield ProviderEvent::Usage { usage: meters, upstream_id: response.id };
                        terminal = true;
                        let completion = if event.kind == "response.incomplete" { "partial" } else { "succeeded" };
                        yield ProviderEvent::Done { completion: Some(completion.to_string()) };
                        break;
                    }
                    "error" | "response.failed" => Err(perr(false, false, "upstream_incomplete"))?,
                    _ => {}
                }
            }
            if !terminal {
                Err(perr(false, false, "stream_interrupted"))?;
            }
        })
    }
}
