//! Tool calling and image input: tool declarations, tool calls, tool results and image parts, with the
//! reference's validation (`packages/contracts`) and the helpers providers share.

use std::collections::{BTreeMap, BTreeSet};

use serde::de::Error as _;
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

use crate::types::Invocation;

/// String limits count code points, as the reference's do.
fn length(s: &str) -> usize {
    s.chars().count()
}

fn is_token(s: &str, max: usize) -> bool {
    (1..=max).contains(&s.len()) && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}

/// `^[a-zA-Z0-9_-]{1,64}$`
pub fn valid_tool_name(s: &str) -> bool {
    is_token(s, 64)
}

/// `^[a-zA-Z0-9_-]{1,128}$`
pub fn valid_tool_call_id(s: &str) -> bool {
    is_token(s, 128)
}

/// One call a model made: `arguments` is the JSON object the model produced, as a string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
    /// An opaque token the upstream attached to this call and wants back when the call is replayed in a later
    /// request (Gemini 3's `thoughtSignature`). It is never interpreted: a caller stores it with the call and sends
    /// it again unchanged, and a provider that has no such token ignores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

/// A function the model may call. `input_schema` is a JSON Schema whose `type` is `"object"`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tool {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub input_schema: Map<String, Value>,
}

/// Whether and which tool the model must call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolChoice {
    /// `"auto"`, `"none"` or `"required"`.
    Mode(String),
    /// `{ "name": ... }`: this declared tool.
    Name(String),
}

impl Serialize for ToolChoice {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Mode(m) => s.serialize_str(m),
            Self::Name(n) => {
                let mut map = s.serialize_map(Some(1))?;
                map.serialize_entry("name", n)?;
                map.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for ToolChoice {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        match Value::deserialize(d)? {
            Value::String(m) if matches!(m.as_str(), "auto" | "none" | "required") => Ok(Self::Mode(m)),
            Value::Object(o) if o.len() == 1 => match o.get("name") {
                Some(Value::String(n)) if valid_tool_name(n) => Ok(Self::Name(n.clone())),
                _ => Err(D::Error::custom("invalid")),
            },
            _ => Err(D::Error::custom("invalid")),
        }
    }
}

/// One part of a user message: text, or an image given as an https URL or a base64 data URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentPart {
    Text { text: String },
    Image { url: String, detail: Option<String> },
}

impl Serialize for ContentPart {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut map = s.serialize_map(None)?;
        match self {
            Self::Text { text } => {
                map.serialize_entry("type", "text")?;
                map.serialize_entry("text", text)?;
            }
            Self::Image { url, detail } => {
                map.serialize_entry("type", "image")?;
                map.serialize_entry("url", url)?;
                if let Some(detail) = detail {
                    map.serialize_entry("detail", detail)?;
                }
            }
        }
        map.end()
    }
}

/// Whether every key of an object is one of `keys`.
fn only(o: &Map<String, Value>, keys: &[&str]) -> bool {
    o.keys().all(|k| keys.contains(&k.as_str()))
}

impl<'de> Deserialize<'de> for ContentPart {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let invalid = || D::Error::custom("invalid");
        let Value::Object(o) = Value::deserialize(d)? else { return Err(invalid()) };
        match o.get("type").and_then(Value::as_str) {
            Some("text") if only(&o, &["type", "text"]) => match o.get("text") {
                Some(Value::String(t)) if (1..=32_000).contains(&length(t)) => Ok(Self::Text { text: t.clone() }),
                _ => Err(invalid()),
            },
            Some("image") if only(&o, &["type", "url", "detail"]) => {
                let url = match o.get("url") {
                    Some(Value::String(u)) if valid_image_url(u) => u.clone(),
                    _ => return Err(invalid()),
                };
                let detail = match o.get("detail") {
                    None => None,
                    Some(Value::String(d)) if matches!(d.as_str(), "auto" | "low" | "high") => Some(d.clone()),
                    _ => return Err(invalid()),
                };
                Ok(Self::Image { url, detail })
            }
            _ => Err(invalid()),
        }
    }
}

/// One chat message. A plain message has `role` `"system"`, `"user"` or `"assistant"` and `content`. A user message
/// may carry `parts` (text and images) instead; an assistant message may carry `tool_calls` (`content` is then
/// optional and empty when absent); a `"tool"` message answers the call `tool_call_id` with `content`, `is_error`
/// marking a failed call.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Message {
    pub role: String,
    pub content: String,
    pub parts: Option<Vec<ContentPart>>,
    pub tool_calls: Option<Vec<ToolCall>>,
    pub tool_call_id: String,
    pub is_error: Option<bool>,
}

impl Message {
    /// A plain text message.
    pub fn text(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self { role: role.into(), content: content.into(), ..Self::default() }
    }
}

impl Serialize for Message {
    /// Exactly the keys the message has, so its canonical form matches the reference.
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut map = s.serialize_map(None)?;
        map.serialize_entry("role", &self.role)?;
        if let Some(parts) = &self.parts {
            map.serialize_entry("content", parts)?;
        } else if !self.content.is_empty() || self.role == "tool" || self.tool_calls.is_none() {
            map.serialize_entry("content", &self.content)?;
        }
        if let Some(calls) = &self.tool_calls {
            map.serialize_entry("tool_calls", calls)?;
        }
        if self.role == "tool" {
            map.serialize_entry("tool_call_id", &self.tool_call_id)?;
            if let Some(e) = self.is_error {
                map.serialize_entry("is_error", &e)?;
            }
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for Message {
    /// The four message shapes of the reference and nothing else.
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let invalid = || D::Error::custom("invalid");
        let Value::Object(o) = Value::deserialize(d)? else { return Err(invalid()) };
        let role = o.get("role").and_then(Value::as_str).ok_or_else(invalid)?.to_string();
        let text = |key: &str, max: usize| match o.get(key) {
            Some(Value::String(s)) if length(s) <= max => Some(s.clone()),
            _ => None,
        };
        let mut out = Message { role: role.clone(), ..Message::default() };
        match role.as_str() {
            "system" | "user" | "assistant" if o.contains_key("tool_calls") => {
                if role != "assistant" || !only(&o, &["role", "content", "tool_calls"]) {
                    return Err(invalid());
                }
                if o.contains_key("content") {
                    out.content = text("content", 32_000).filter(|s| !s.is_empty()).ok_or_else(invalid)?;
                }
                let calls: Vec<ToolCall> = serde_json::from_value(o["tool_calls"].clone()).map_err(|_| invalid())?;
                let calls_ok = calls.iter().all(|c| {
                    valid_tool_call_id(&c.id)
                        && valid_tool_name(&c.name)
                        && length(&c.arguments) <= 256_000
                        && is_json_object(&c.arguments)
                        && c.signature.as_deref().is_none_or(valid_signature)
                });
                if !(1..=64).contains(&calls.len()) || !calls_ok {
                    return Err(invalid());
                }
                out.tool_calls = Some(calls);
            }
            "system" | "user" | "assistant" => {
                if !o.contains_key("content") || !only(&o, &["role", "content"]) {
                    return Err(invalid());
                }
                match &o["content"] {
                    Value::String(s) if (1..=32_000).contains(&length(s)) => out.content = s.clone(),
                    Value::Array(_) if role == "user" => {
                        let parts: Vec<ContentPart> = serde_json::from_value(o["content"].clone()).map_err(|_| invalid())?;
                        if !(1..=32).contains(&parts.len()) {
                            return Err(invalid());
                        }
                        out.parts = Some(parts);
                    }
                    _ => return Err(invalid()),
                }
            }
            "tool" => {
                if !only(&o, &["role", "tool_call_id", "content", "is_error"]) {
                    return Err(invalid());
                }
                out.tool_call_id = text("tool_call_id", 128).filter(|s| valid_tool_call_id(s)).ok_or_else(invalid)?;
                out.content = text("content", 256_000).ok_or_else(invalid)?;
                out.is_error = match o.get("is_error") {
                    None => None,
                    Some(Value::Bool(b)) => Some(*b),
                    Some(_) => return Err(invalid()),
                };
            }
            _ => return Err(invalid()),
        }
        Ok(out)
    }
}

/// JavaScript's `\s`: WhiteSpace and LineTerminator code points.
pub fn is_js_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}'
    )
}

/// JavaScript's `String.prototype.trim`, which differs from `str::trim` (U+FEFF is trimmed, U+0085 is not).
pub fn js_trim(s: &str) -> &str {
    s.trim_matches(is_js_space)
}

/// An `https:` URL of at most 4096 UTF-16 units with no whitespace, or a base64 data URL of a png, jpeg, gif or webp
/// image (at most 3,000,000 code points).
pub fn valid_image_url(u: &str) -> bool {
    if length(u) > 3_000_000 {
        return false;
    }
    if let Some(rest) = u.strip_prefix("data:image/") {
        let Some((kind, data)) = rest.split_once(";base64,") else { return false };
        let body = data.trim_end_matches('=');
        let base64 = |c: u8| c.is_ascii_alphanumeric() || c == b'+' || c == b'/';
        return matches!(kind, "png" | "jpeg" | "gif" | "webp")
            && data.len() - body.len() <= 2
            && !body.is_empty()
            && body.bytes().all(base64);
    }
    match u.strip_prefix("https://") {
        Some(rest) => u.encode_utf16().count() <= 4096 && !rest.is_empty() && !rest.chars().any(is_js_space),
        None => false,
    }
}

/// Whether `s` parses as a JSON object, as `JSON.parse` would.
pub fn is_json_object(s: &str) -> bool {
    serde_json::from_str::<Value>(s).is_ok_and(|v| v.is_object())
}

/// Every value at most `limit` containers deep, as the reference counts.
fn json_depth(v: &Value, limit: i32) -> bool {
    if limit < 0 {
        return false;
    }
    match v {
        Value::Array(items) => items.iter().all(|e| json_depth(e, limit - 1)),
        Value::Object(m) => m.values().all(|e| json_depth(e, limit - 1)),
        _ => true,
    }
}

fn valid_tool(t: &Tool) -> bool {
    let description_ok = t.description.as_deref().is_none_or(|d| (1..=4096).contains(&length(d)));
    let schema = Value::Object(t.input_schema.clone());
    valid_tool_name(&t.name) && description_ok && t.input_schema.get("type") == Some(&Value::from("object")) && json_depth(&schema, 32)
}

/// The cross-field rules: unique tool names, `tool_choice` only with tools and naming a declared one, no tools on a
/// task, and every tool result answering an earlier, still open call of the step before it.
pub(crate) fn valid_tool_use(i: &Invocation) -> bool {
    let input = &i.input;
    if let Some(tools) = &input.tools {
        if !(1..=64).contains(&tools.len()) || !tools.iter().all(valid_tool) {
            return false;
        }
    }
    let names: BTreeSet<&str> = input.tools.iter().flatten().map(|t| t.name.as_str()).collect();
    if names.len() != input.tools.as_ref().map_or(0, Vec::len) {
        return false;
    }
    if input.task.is_some() && (input.tools.is_some() || input.tool_choice.is_some()) {
        return false;
    }
    match &input.tool_choice {
        Some(_) if input.tools.is_none() => return false,
        Some(ToolChoice::Name(n)) if !names.contains(n.as_str()) => return false,
        _ => {}
    }
    let mut seen = BTreeSet::new();
    let mut open = BTreeSet::new();
    for m in &input.messages {
        if m.role == "tool" {
            if !open.remove(&m.tool_call_id) {
                return false;
            }
            continue;
        }
        if !open.is_empty() {
            return false;
        }
        for c in m.tool_calls.iter().flatten() {
            if !seen.insert(c.id.clone()) {
                return false;
            }
            open.insert(c.id.clone());
        }
    }
    open.is_empty()
}

/// What a request needs from a provider beyond plain text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Needs {
    pub tools: bool,
    pub images: bool,
    /// The request carries a `response_format`: the answer must follow a JSON Schema.
    pub structured_output: bool,
    pub temperature: bool,
    pub top_p: bool,
    pub stop: bool,
    pub seed: bool,
    pub reasoning_effort: bool,
}

/// Whether a request declares tools or carries tool history, whether it carries images, and whether it asks for
/// structured output.
pub fn invocation_needs(i: &Invocation) -> Needs {
    let messages = &i.input.messages;
    Needs {
        tools: i.input.tools.is_some() || messages.iter().any(|m| m.role == "tool" || m.tool_calls.is_some()),
        images: messages.iter().flat_map(|m| m.parts.iter().flatten()).any(|p| matches!(p, ContentPart::Image { .. })),
        structured_output: i.response_format.is_some(),
        temperature: i.sampling.as_ref().is_some_and(|s| s.temperature.is_some()),
        top_p: i.sampling.as_ref().is_some_and(|s| s.top_p.is_some()),
        stop: i.sampling.as_ref().is_some_and(|s| s.stop.is_some()),
        seed: i.sampling.as_ref().is_some_and(|s| s.seed.is_some()),
        reasoning_effort: i.reasoning_effort.is_some(),
    }
}

/// What a provider can carry besides text. The default means text only, so a request with tools or images is refused
/// before any charge rather than silently losing them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProviderCapabilities {
    pub tools: bool,
    pub images: bool,
    /// The provider writes a request's `response_format` schema into its call. A provider that does not say so is
    /// never sent one: the answer would silently ignore the schema.
    pub structured_output: bool,
    /// Each sampling control and the reasoning effort is carried only when the provider says so: otherwise the request
    /// is refused before reserving, never sent without it.
    pub temperature: bool,
    pub top_p: bool,
    pub stop: bool,
    pub seed: bool,
    pub reasoning_effort: bool,
}

impl ProviderCapabilities {
    /// Whether these capabilities cover a request's needs.
    pub fn can(&self, n: Needs) -> bool {
        (!n.tools || self.tools)
            && (!n.images || self.images)
            && (!n.structured_output || self.structured_output)
            && (!n.temperature || self.temperature)
            && (!n.top_p || self.top_p)
            && (!n.stop || self.stop)
            && (!n.seed || self.seed)
            && (!n.reasoning_effort || self.reasoning_effort)
    }
}

/// Re-encodes JSON text the way `JSON.stringify(JSON.parse(text))` does: no whitespace, keys in their original order,
/// numbers and string escapes as JavaScript writes them. Providers use it when an upstream hands over a tool call's
/// arguments as a parsed object, so every port reports the same arguments string.
pub fn js_stringify(text: &str) -> Result<String, serde_json::Error> {
    let mut out = String::new();
    let mut de = serde_json::Deserializer::from_str(text);
    serde::de::DeserializeSeed::deserialize(Js(&mut out), &mut de)?;
    de.end()?;
    Ok(out)
}

/// Writes the value it visits, in document order.
struct Js<'a>(&'a mut String);

impl<'de> serde::de::DeserializeSeed<'de> for Js<'_> {
    type Value = ();
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> serde::de::Visitor<'de> for Js<'_> {
    type Value = ();
    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("JSON")
    }
    fn visit_bool<E>(self, b: bool) -> Result<(), E> {
        self.0.push_str(if b { "true" } else { "false" });
        Ok(())
    }
    #[allow(clippy::cast_precision_loss)]
    fn visit_i64<E>(self, n: i64) -> Result<(), E> {
        self.0.push_str(&crate::parse::js_number(n as f64));
        Ok(())
    }
    #[allow(clippy::cast_precision_loss)]
    fn visit_u64<E>(self, n: u64) -> Result<(), E> {
        self.0.push_str(&crate::parse::js_number(n as f64));
        Ok(())
    }
    fn visit_f64<E>(self, n: f64) -> Result<(), E> {
        self.0.push_str(&crate::parse::js_number(n));
        Ok(())
    }
    fn visit_str<E>(self, s: &str) -> Result<(), E> {
        crate::parse::write_string(self.0, s);
        Ok(())
    }
    fn visit_unit<E>(self) -> Result<(), E> {
        self.0.push_str("null");
        Ok(())
    }
    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        self.0.push('[');
        let mut first = true;
        loop {
            let at = self.0.len();
            if !first {
                self.0.push(',');
            }
            if seq.next_element_seed(Js(&mut *self.0))?.is_none() {
                self.0.truncate(at);
                break;
            }
            first = false;
        }
        self.0.push(']');
        Ok(())
    }
    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        // JSON.parse keeps the last of duplicate keys at the place of the first, and a JavaScript object lists
        // its array-index keys first, in numeric order.
        let mut fields: Vec<(String, String)> = Vec::new();
        while let Some(key) = map.next_key::<String>()? {
            let mut value = String::new();
            map.next_value_seed(Js(&mut value))?;
            match fields.iter_mut().find(|(k, _)| *k == key) {
                Some(field) => field.1 = value,
                None => fields.push((key, value)),
            }
        }
        fields.sort_by_key(|(k, _)| array_index(k).map_or((1, 0), |i| (0, i)));
        self.0.push('{');
        for (i, (key, value)) in fields.iter().enumerate() {
            if i > 0 {
                self.0.push(',');
            }
            crate::parse::write_string(self.0, key);
            self.0.push(':');
            self.0.push_str(value);
        }
        self.0.push('}');
        Ok(())
    }
}

/// The number a key names when JavaScript treats it as an array index: a canonical integer below 2^32 - 1.
fn array_index(key: &str) -> Option<u32> {
    let canonical = key == "0" || (!key.starts_with('0') && !key.is_empty() && key.bytes().all(|b| b.is_ascii_digit()));
    canonical.then(|| key.parse::<u32>().ok()).flatten().filter(|n| *n < u32::MAX)
}

/// Collected tool calls of one answer, by index.
pub type ToolCalls = BTreeMap<usize, ToolCall>;

/// A call signature is opaque text of 1 to 16384 characters with no whitespace or control characters (providers send
/// base64 or similar).
pub fn valid_signature(s: &str) -> bool {
    (1..=16_384).contains(&s.chars().count()) && s.chars().all(|c| !c.is_whitespace() && !c.is_control())
}
