//! The wire types shared by every Super Router component. They mirror `packages/contracts` of the TypeScript
//! reference and the Go `contracts` package. Use [`crate::parse_service`] and [`crate::parse_invocation`] to
//! decode untrusted JSON with the reference's validation.

use std::collections::BTreeMap;

use futures_core::stream::BoxStream;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};
use super_router_pricing::{valid_unit_name, Price, Usage, MEDIA_KEYS};

pub use crate::tools::{ContentPart, Message, ProviderCapabilities, Tool, ToolCall, ToolChoice};

pub use super_router_pricing::RouterError;

/// Media meter names, in the order the reference iterates them.
pub const MEDIA_METERS: [&str; 4] = MEDIA_KEYS;

/// The media meter that bills a task kind.
pub fn kind_meter(kind: &str) -> Option<&'static str> {
    match kind {
        "image" => Some("images"),
        "video" => Some("video_ms"),
        "audio" => Some("audio_ms"),
        "task" => Some("tasks"),
        _ => None,
    }
}

pub fn is_media(key: &str) -> bool {
    MEDIA_METERS.contains(&key)
}

/// The scopes a member role's keys may hold.
pub fn role_scopes(role: &str) -> Option<&'static [&'static str]> {
    Some(match role {
        "owner" => &["invoke", "identity:write", "service:write", "routing:write", "billing:write", "billing:read", "operations:read"],
        "service_admin" => &["invoke", "service:write", "routing:write", "operations:read"],
        "billing_admin" => &["billing:write", "billing:read", "operations:read"],
        "viewer" => &["billing:read", "operations:read"],
        "app" => &["invoke", "credits:read"],
        _ => return None,
    })
}

/// A failed upstream call. `definitive` means the provider certainly did no work, so the reservation can be
/// released; `retryable` means another offering may be tried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderError {
    pub definitive: bool,
    pub retryable: bool,
    pub code: String,
    /// How long the provider asked callers to wait before trying again (its `Retry-After`), in milliseconds. Only
    /// a hint: a health book that cools the offering treats it as a minimum, never as a promise.
    pub retry_after_ms: Option<u64>,
}

impl ProviderError {
    /// An outcome the provider may or may not have acted on: funds stay held.
    pub fn unknown(code: impl Into<String>) -> Self {
        Self { definitive: false, retryable: false, code: code.into(), retry_after_ms: None }
    }

    /// The provider certainly did nothing; `retryable` allows the next offering to be tried.
    pub fn definitive(code: impl Into<String>, retryable: bool) -> Self {
        Self { definitive: true, retryable, code: code.into(), retry_after_ms: None }
    }

    /// The same error carrying the wait the provider asked for. `None` leaves it as it was.
    #[must_use]
    pub fn with_retry_after(mut self, retry_after_ms: Option<u64>) -> Self {
        self.retry_after_ms = retry_after_ms;
        self
    }
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.code)
    }
}

impl std::error::Error for ProviderError {}

/// An authenticated caller. Hosts must only construct one from verified credentials.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Principal {
    pub workspace: String,
    pub environment: String,
    pub customer: String,
    pub scopes: Vec<String>,
    pub auth_key_id: String,
    pub member_id: String,
    /// Limits the principal to these services. `None` means unrestricted.
    pub service_ids: Option<Vec<String>>,
}

impl Principal {
    pub fn has_scope(&self, scope: &str) -> bool {
        self.scopes.iter().any(|s| s == scope)
    }

    pub fn may_use(&self, service: &str) -> bool {
        self.service_ids.as_ref().is_none_or(|ids| ids.iter().any(|s| s == service))
    }
}

/// Media and custom-unit limits. Media meters sit at the top level of the wire object and custom units under
/// `units`; in memory both are one flat map.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MeterLimits(pub BTreeMap<String, i64>);

impl MeterLimits {
    pub fn get(&self, key: &str) -> i64 {
        self.0.get(key).copied().unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &i64)> {
        self.0.iter()
    }
}

pub(crate) const MAX_SAFE: i64 = (1 << 53) - 1;

pub(crate) fn safe_non_neg(n: i64) -> bool {
    (0..=MAX_SAFE).contains(&n)
}

impl<'de> Deserialize<'de> for MeterLimits {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = Map::<String, Value>::deserialize(d)?;
        let mut out = BTreeMap::new();
        for (k, v) in raw {
            if k == "units" {
                let units = v.as_object().ok_or_else(|| D::Error::custom("invalid"))?;
                for (name, n) in units {
                    let n = n.as_i64().filter(|n| safe_non_neg(*n));
                    match n {
                        Some(n) if valid_unit_name(name) => out.insert(name.clone(), n),
                        _ => return Err(D::Error::custom("invalid")),
                    };
                }
            } else {
                match v.as_i64() {
                    Some(n) if is_media(&k) && safe_non_neg(n) => out.insert(k, n),
                    _ => return Err(D::Error::custom("invalid")),
                };
            }
        }
        Ok(Self(out))
    }
}

impl Serialize for MeterLimits {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut out = Map::new();
        let mut units = Map::new();
        for (k, n) in &self.0 {
            if is_media(k) {
                out.insert(k.clone(), Value::from(*n));
            } else {
                units.insert(k.clone(), Value::from(*n));
            }
        }
        if !units.is_empty() {
            out.insert("units".into(), Value::Object(units));
        }
        Value::Object(out).serialize(s)
    }
}

/// Identifies the evaluation behind an offering's quality score.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualityMetadata {
    pub source: String,
    pub task: String,
    pub updated_at: String,
}

/// Provenance and validity of a handling or compliance label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LabelEvidence {
    pub source: String,
    pub verification: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}

/// An enforced JSON Schema response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JsonSchema {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub schema: Map<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResponseFormat {
    #[serde(rename = "type")]
    pub kind: String,
    pub json_schema: JsonSchema,
}

/// Explicitly allows supply in an operator-designated lower tier.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DowngradePolicy {
    pub tier: String,
    pub offerings: Vec<Offering>,
}

/// One concrete model route a service can use.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Offering {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality_metadata: Option<QualityMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label_evidence: Option<BTreeMap<String, LabelEvidence>>,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_kind: Option<String>,
    #[serde(default, skip_serializing_if = "MeterLimits::is_empty")]
    pub max_usage: MeterLimits,
    pub provider: String,
    pub model: String,
    pub upstream_model: String,
    pub region: String,
    #[serde(default)]
    pub labels: Vec<String>,
    pub enabled: bool,
    pub source: String,
    pub max_input_tokens: i64,
    pub max_output_tokens: i64,
    pub price: Price,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reliability: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality: Option<f64>,
}

impl Offering {
    pub fn has_capability(&self, name: &str) -> bool {
        self.capabilities.as_ref().is_some_and(|c| c.iter().any(|x| x == name))
    }

    pub fn label_evidence(&self, label: &str) -> Option<&LabelEvidence> {
        self.label_evidence.as_ref().and_then(|e| e.get(label))
    }
}

/// The scoring weights; they sum to one.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Weights {
    pub cost: f64,
    pub latency: f64,
    pub reliability: f64,
    pub quality: f64,
}

fn is_empty_str(s: &str) -> bool {
    s.is_empty()
}

/// A published, versioned product a developer sells.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Service {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub downgrade_policy: Option<DowngradePolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub require_verified_labels: Option<bool>,
    pub id: String,
    pub version: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_kind: Option<String>,
    pub mode: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub model: String,
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub tier: String,
    pub offerings: Vec<Offering>,
    pub regions: Vec<String>,
    #[serde(default)]
    pub required_labels: Vec<String>,
    pub price: Price,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform_price: Option<Price>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_subscription: Option<bool>,
    pub max_attempts: i64,
    pub max_provider_cost: String,
    pub max_output_tokens: i64,
    pub weights: Weights,
    pub cost_scale: String,
    pub latency_scale_ms: f64,
    #[serde(default)]
    pub unknown_score: f64,
}

impl Service {
    /// Primary and explicitly allowed downgrade routes, in that order.
    pub fn all_offerings(&self) -> Vec<&Offering> {
        let mut all: Vec<&Offering> = self.offerings.iter().collect();
        if let Some(d) = &self.downgrade_policy {
            all.extend(d.offerings.iter());
        }
        all
    }
}

/// A bounded media or tool task instead of chat messages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Task {
    #[serde(default, skip_serializing_if = "is_empty_str")]
    pub meter: String,
    pub kind: String,
    pub prompt: String,
}

impl Task {
    /// The custom meter name when present, or the built-in meter of the kind.
    pub fn meter_name(&self) -> String {
        if self.meter.is_empty() {
            kind_meter(&self.kind).unwrap_or_default().to_string()
        } else {
            self.meter.clone()
        }
    }
}

/// Caps what a customer may be charged for one request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChargeLimit {
    pub asset: String,
    pub atoms: String,
    pub scale: i64,
}

fn one() -> i64 {
    1
}

fn thirty_seconds() -> i64 {
    30_000
}

fn max_usage_omitted(m: &Option<MeterLimits>) -> bool {
    m.as_ref().is_none_or(MeterLimits::is_empty)
}

/// Bounds one invocation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    #[serde(default = "one")]
    pub max_output_tokens: i64,
    /// `Some` with no entries is not the same as `None`: a chat request must not carry media limits at all.
    #[serde(default, skip_serializing_if = "max_usage_omitted")]
    pub max_usage: Option<MeterLimits>,
    #[serde(default = "thirty_seconds")]
    pub timeout_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_charge: Option<ChargeLimit>,
}

impl Limits {
    pub fn usage_limit(&self, meter: &str) -> i64 {
        self.max_usage.as_ref().map_or(0, |m| m.get(meter))
    }
}

/// What an invocation asks for: chat messages (optionally with tools), or a bounded task. `tools` and `tool_choice`
/// are `None` when absent, so a request without tools keeps its exact canonical form and digest.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    #[serde(default)]
    pub messages: Vec<Message>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Tool>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<Task>,
}

fn developer() -> String {
    "developer".to_string()
}

/// One request to run a service.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Invocation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_format: Option<ResponseFormat>,
    pub service: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_version: Option<i64>,
    pub customer_id: String,
    #[serde(default = "developer")]
    pub payer_mode: String,
    pub input: Input,
    pub limits: Limits,
    #[serde(default)]
    pub stream: bool,
}

/// A usage record reported by a provider. Media meters are `None` when not reported.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Meters {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub units: BTreeMap<String, i64>,
    #[serde(default)]
    pub input: i64,
    #[serde(default)]
    pub cached: i64,
    #[serde(default)]
    pub cache_write: i64,
    #[serde(default)]
    pub output: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tasks: Option<i64>,
}

impl Meters {
    /// The pricing representation.
    pub fn usage(&self) -> Usage {
        let mut media = BTreeMap::new();
        for key in MEDIA_METERS {
            if let Some(n) = self.media_reading(key) {
                media.insert(key.to_string(), n);
            }
        }
        Usage {
            units: self.units.clone(),
            input: self.input,
            cached: self.cached,
            cache_write: self.cache_write,
            output: self.output,
            media,
        }
    }

    fn media_reading(&self, key: &str) -> Option<i64> {
        match key {
            "images" => self.images,
            "video_ms" => self.video_ms,
            "audio_ms" => self.audio_ms,
            "tasks" => self.tasks,
            _ => None,
        }
    }

    /// One media meter, zero when not reported.
    pub fn media(&self, key: &str) -> i64 {
        self.media_reading(key).unwrap_or(0)
    }

    /// Custom readings merged with every built-in media meter.
    pub fn quantities(&self) -> BTreeMap<String, i64> {
        let mut out: BTreeMap<String, i64> = MEDIA_METERS.iter().map(|k| ((*k).to_string(), self.media(k))).collect();
        out.extend(self.units.iter().map(|(k, n)| (k.clone(), *n)));
        out
    }

    /// Whether every meter is a non-negative safe integer, as the reference requires of provider usage.
    pub fn is_valid(&self) -> bool {
        self.units.iter().all(|(k, n)| valid_unit_name(k) && safe_non_neg(*n))
            && [self.input, self.cached, self.cache_write, self.output].into_iter().all(safe_non_neg)
            && [self.images, self.video_ms, self.audio_ms, self.tasks].into_iter().flatten().all(safe_non_neg)
    }
}

/// One step of a provider's answer: a text delta, part of a tool call, the final usage, or the end.
#[derive(Debug, Clone, PartialEq)]
pub enum ProviderEvent {
    Delta {
        text: String,
    },
    /// Part of a tool call. Calls are numbered from zero by `index`; the first event of an index carries `id` and
    /// `name` (and `signature`, when the upstream attached one), and `arguments_delta` is the next piece of the
    /// call's JSON arguments.
    ToolCall {
        index: usize,
        id: Option<String>,
        name: Option<String>,
        signature: Option<String>,
        arguments_delta: String,
    },
    Usage {
        usage: Meters,
        upstream_id: String,
    },
    /// `completion` is `"succeeded"` (the default when `None`), `"partial"` (cut short) or `"filtered"` (cut short by
    /// the provider's content policy: recorded as partial, reported to the caller as the finish reason
    /// `content_filter` rather than `length`).
    Done {
        completion: Option<String>,
    },
}

/// What a provider produces: events in order, ending after `Done` or with the first error. Dropping the stream
/// abandons the upstream call.
pub type ProviderStream = BoxStream<'static, Result<ProviderEvent, ProviderError>>;

/// Runs one invocation against an upstream model.
pub trait Provider: Send + Sync {
    fn invoke(&self, input: &Invocation, offering: &Offering) -> ProviderStream;

    /// What the provider can carry besides text. The default is text only: a request with tools or images is then
    /// refused before anything is reserved.
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }
}

/// One server-sent event of a streaming inference.
#[derive(Debug, Clone, PartialEq)]
pub struct StreamEvent {
    pub kind: String,
    pub request_id: String,
    pub sequence: u32,
    pub data: Value,
}
