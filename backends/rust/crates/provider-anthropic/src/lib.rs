//! The [`Provider`] for the Anthropic Messages API. It streams text, normalises usage into router meters and
//! classifies failures like the OpenAI provider: redirects are never followed, keys never appear in errors, and
//! a completed message whose usage is missing or malformed is refused rather than billed as free.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use async_stream::try_stream;
use futures_util::StreamExt;
use reqwest::header::{HeaderName, HeaderValue};
use reqwest::{redirect, Client, ClientBuilder};
use serde::{Deserialize, Deserializer};
use serde_json::{json, Map, Value};
use super_router_contracts::{Invocation, Meters, Offering, Provider, ProviderError, ProviderEvent, ProviderStream};

const DEFAULT_BASE: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2023-06-01";

/// Supplies the API key on every call, so it can rotate. An empty key is a definitive failure.
pub type KeySource = Arc<dyn Fn() -> String + Send + Sync>;

fn perr(definitive: bool, retryable: bool, code: &str) -> ProviderError {
    ProviderError { definitive, retryable, code: code.to_string() }
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
}

fn parse_event(data: &str) -> Result<Event, ProviderError> {
    let invalid = || perr(false, false, "invalid_upstream_event");
    let value: Value = serde_json::from_str(data).map_err(|_| invalid())?;
    if !value.is_object() {
        return Err(invalid());
    }
    serde_json::from_value(value).map_err(|_| invalid())
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

fn classify_status(status: u16) -> ProviderError {
    let code = format!("upstream_http_{status}");
    match status {
        400 | 401 | 403 | 404 | 422 => perr(true, false, &code),
        429 => perr(true, true, &code),
        _ => perr(false, false, &code),
    }
}

impl Provider for Anthropic {
    fn invoke(&self, input: &Invocation, offering: &Offering) -> ProviderStream {
        let this = self.clone();
        let is_task = input.input.task.is_some();
        let mut system: Vec<&str> = Vec::new();
        let mut turns = Vec::new();
        for m in &input.input.messages {
            if m.role == "system" {
                system.push(&m.content);
            } else {
                turns.push(json!({"role": m.role, "content": m.content}));
            }
        }
        let system = system.join("\n");
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
            if turns.is_empty() {
                Err(perr(true, false, "messages_required"))?;
            }
            // A key that cannot be a header value never leaves the process, so nothing was dispatched.
            let mut api_key = HeaderValue::from_str(&key).map_err(|_| perr(true, false, "request_invalid"))?;
            api_key.set_sensitive(true);
            let mut body = json!({"model": model, "max_tokens": max_tokens, "messages": turns, "stream": true});
            if !system.is_empty() {
                body["system"] = Value::String(system);
            }
            let mut request = this
                .client
                .post(format!("{}/v1/messages", this.base))
                .header(HeaderName::from_static("x-api-key"), api_key)
                .header("anthropic-version", API_VERSION)
                .json(&body);
            if let Some(timeout) = timeout {
                request = request.timeout(timeout);
            }
            let response = request.send().await.map_err(|_| perr(false, false, "transport_unknown"))?;
            let status = response.status().as_u16();
            if !(200..=299).contains(&status) {
                Err(classify_status(status))?;
            }
            let mut frames = Box::pin(super_router_sse::frames(response.bytes_stream()));
            let mut current: Option<Usage> = None;
            let mut id = String::new();
            let mut completion = "succeeded";
            let mut terminal = false;
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
                    "content_block_delta" => {
                        if let Some(delta) = event.delta.filter(|d| d.kind == "text_delta") {
                            yield ProviderEvent::Delta { text: delta.text };
                        }
                    }
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
