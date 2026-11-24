//! The request side of `generateContent`: how messages, tools and the tool choice are written.
//!
//! Gemini differs from the Responses and Messages APIs in four places this module absorbs: the assistant role is
//! `model`, the system prompt travels as `systemInstruction`, a tool result names the function it answers (not the
//! call), and tool-call arguments are a JSON object (not a string).

use std::collections::HashMap;

use serde_json::{json, Map, Value};
use super_router_contracts::{ContentPart, Input, Message, ToolChoice};

/// Why a request was not written. Every one is a request the upstream would reject, so nothing was dispatched.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// A tool call's arguments are not a JSON object, or a tool result names a call that was never made.
    Invalid,
    /// An image that is not a `data:` URL: Gemini fetches only its own file URIs, so a link cannot be sent.
    ImageUrl,
}

/// Converts messages to the `contents` list. System messages are left out (they travel as `systemInstruction`),
/// tool calls become `functionCall` parts and the results of one step travel together as `functionResponse` parts in
/// a single user turn.
pub(crate) fn contents(messages: &[Message]) -> Result<Vec<Value>, Refusal> {
    let mut out: Vec<Value> = Vec::with_capacity(messages.len());
    // The name of every tool call seen so far, by id: a result carries only the id.
    let mut names: HashMap<&str, &str> = HashMap::new();
    let mut in_results = false;
    for m in messages {
        if m.role == "system" {
            continue;
        }
        if m.role == "tool" {
            let name = names.get(m.tool_call_id.as_str()).ok_or(Refusal::Invalid)?;
            let key = if m.is_error == Some(true) { "error" } else { "output" };
            let part = json!({ "functionResponse": { "name": name, "response": { key: m.content } } });
            match (in_results, out.last_mut().and_then(|turn| turn["parts"].as_array_mut())) {
                (true, Some(parts)) => parts.push(part),
                _ => {
                    out.push(json!({ "role": "user", "parts": [part] }));
                    in_results = true;
                }
            }
            continue;
        }
        in_results = false;
        let role = if m.role == "assistant" { "model" } else { "user" };
        if let Some(calls) = &m.tool_calls {
            let mut parts = Vec::with_capacity(calls.len() + 1);
            if !m.content.is_empty() {
                parts.push(json!({ "text": m.content }));
            }
            for c in calls {
                names.insert(c.id.as_str(), c.name.as_str());
                parts.push(json!({ "functionCall": { "name": c.name, "args": args(&c.arguments)? } }));
            }
            out.push(json!({ "role": role, "parts": parts }));
        } else if let Some(parts) = &m.parts {
            let parts = parts
                .iter()
                .map(|p| match p {
                    ContentPart::Text { text } => Ok(json!({ "text": text })),
                    ContentPart::Image { url, .. } => {
                        let (media, data) =
                            url.strip_prefix("data:").and_then(|rest| rest.split_once(";base64,")).ok_or(Refusal::ImageUrl)?;
                        Ok(json!({ "inlineData": { "mimeType": media, "data": data } }))
                    }
                })
                .collect::<Result<Vec<_>, Refusal>>()?;
            out.push(json!({ "role": role, "parts": parts }));
        } else {
            out.push(json!({ "role": role, "parts": [{ "text": m.content }] }));
        }
    }
    Ok(out)
}

/// A call's arguments as the object Gemini wants. No arguments at all is an empty object.
fn args(arguments: &str) -> Result<Value, Refusal> {
    if arguments.trim().is_empty() {
        return Ok(json!({}));
    }
    match serde_json::from_str::<Value>(arguments) {
        Ok(v @ Value::Object(_)) => Ok(v),
        _ => Err(Refusal::Invalid),
    }
}

/// Adds `tools` and `toolConfig` to a request body, and nothing when the request declares no tools.
///
/// The schema travels as `parametersJsonSchema`, which takes JSON Schema as written. The older `parameters` field
/// accepts only an OpenAPI subset and would need a translator that silently drops keywords.
pub(crate) fn tool_fields(body: &mut Map<String, Value>, input: &Input) {
    if let Some(tools) = &input.tools {
        let declarations: Vec<Value> = tools
            .iter()
            .map(|t| {
                let mut declaration = json!({ "name": t.name, "parametersJsonSchema": t.input_schema });
                if let Some(d) = &t.description {
                    declaration["description"] = json!(d);
                }
                declaration
            })
            .collect();
        body.insert("tools".into(), json!([{ "functionDeclarations": declarations }]));
    }
    let config = match &input.tool_choice {
        None => return,
        Some(ToolChoice::Name(name)) => json!({ "mode": "ANY", "allowedFunctionNames": [name] }),
        Some(ToolChoice::Mode(mode)) => match mode.as_str() {
            "none" => json!({ "mode": "NONE" }),
            "required" => json!({ "mode": "ANY" }),
            _ => json!({ "mode": "AUTO" }),
        },
    };
    body.insert("toolConfig".into(), json!({ "functionCallingConfig": config }));
}
