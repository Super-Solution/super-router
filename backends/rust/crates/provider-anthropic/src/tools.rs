//! Tool calling and image parts on the Messages API: how messages, tools and the tool choice are written, and
//! how the numbers of a streamed event are read.

use serde::Serialize;
use serde_json::value::RawValue;
use serde_json::{json, Map, Value};
use super_router_contracts::{js_stringify, ContentPart, Input, Message, ToolChoice};

/// One turn of the `messages` list.
#[derive(Serialize)]
pub(crate) struct Turn {
    role: String,
    content: Content,
}

#[derive(Serialize)]
#[serde(untagged)]
enum Content {
    Text(String),
    Blocks(Vec<Block>),
}

/// A content block. A tool call's input is written as the caller's own JSON, in its own key order.
#[derive(Serialize)]
#[serde(untagged)]
enum Block {
    Json(Value),
    ToolUse {
        #[serde(rename = "type")]
        kind: &'static str,
        id: String,
        name: String,
        input: Box<RawValue>,
    },
}

/// Converts messages to the Messages API `messages` list. System messages are left out (they travel as `system`),
/// plain text messages pass through unchanged, tool calls become `tool_use` blocks and the results of one step
/// travel together as `tool_result` blocks in a single user message.
///
/// Fails only when a tool call's arguments are not JSON, which a parsed invocation never holds.
pub(crate) fn messages(messages: &[Message]) -> Result<Vec<Turn>, serde_json::Error> {
    let mut out: Vec<Turn> = Vec::new();
    let mut in_results = false;
    for m in messages {
        if m.role == "system" {
            continue;
        }
        if m.role == "tool" {
            let mut block = json!({ "type": "tool_result", "tool_use_id": m.tool_call_id, "content": m.content });
            if m.is_error == Some(true) {
                block["is_error"] = json!(true);
            }
            if let (true, Some(Turn { content: Content::Blocks(blocks), .. })) = (in_results, out.last_mut()) {
                blocks.push(Block::Json(block));
            } else {
                out.push(Turn { role: "user".into(), content: Content::Blocks(vec![Block::Json(block)]) });
                in_results = true;
            }
            continue;
        }
        in_results = false;
        if let Some(calls) = &m.tool_calls {
            let mut blocks = Vec::with_capacity(calls.len() + 1);
            if !m.content.is_empty() {
                blocks.push(Block::Json(json!({ "type": "text", "text": m.content })));
            }
            for c in calls {
                // Re-encoded as JSON.parse then JSON.stringify would, so every port sends the same bytes.
                let input = RawValue::from_string(js_stringify(&c.arguments)?)?;
                blocks.push(Block::ToolUse { kind: "tool_use", id: c.id.clone(), name: c.name.clone(), input });
            }
            out.push(Turn { role: "assistant".into(), content: Content::Blocks(blocks) });
        } else if let Some(parts) = &m.parts {
            let blocks = parts
                .iter()
                .map(|p| match p {
                    ContentPart::Text { text } => Block::Json(json!({ "type": "text", "text": text })),
                    ContentPart::Image { url, .. } => {
                        let source = match url.strip_prefix("data:").and_then(|rest| rest.split_once(";base64,")) {
                            Some((media, data)) => json!({ "type": "base64", "media_type": media, "data": data }),
                            None => json!({ "type": "url", "url": url }),
                        };
                        Block::Json(json!({ "type": "image", "source": source }))
                    }
                })
                .collect();
            out.push(Turn { role: m.role.clone(), content: Content::Blocks(blocks) });
        } else {
            out.push(Turn { role: m.role.clone(), content: Content::Text(m.content.clone()) });
        }
    }
    Ok(out)
}

/// Adds `tools` and `tool_choice` to a request body, and nothing when the request declares no tools.
pub(crate) fn tool_fields(body: &mut Map<String, Value>, input: &Input) {
    if let Some(tools) = &input.tools {
        let tools: Vec<Value> = tools
            .iter()
            .map(|t| {
                let mut tool = json!({ "name": t.name, "input_schema": t.input_schema });
                if let Some(d) = &t.description {
                    tool["description"] = json!(d);
                }
                tool
            })
            .collect();
        body.insert("tools".into(), Value::Array(tools));
    }
    let choice = match &input.tool_choice {
        None => return,
        Some(ToolChoice::Name(name)) => json!({ "type": "tool", "name": name }),
        Some(ToolChoice::Mode(mode)) if mode == "required" => json!({ "type": "any" }),
        Some(ToolChoice::Mode(mode)) => json!({ "type": mode }),
    };
    body.insert("tool_choice".into(), choice);
}

/// Reads a JSON number that is a safe integer, as `Number.isSafeInteger` accepts.
pub(crate) fn safe_index(v: Option<&Value>) -> Option<i64> {
    let f = v?.as_f64()?;
    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    (f.trunc() == f && f.abs() <= ((1_i64 << 53) - 1) as f64).then_some(f as i64)
}

/// Reads a JSON string; null is not one.
pub(crate) fn text(v: Option<&Value>) -> Option<String> {
    v?.as_str().map(str::to_string)
}

/// A `tool_use` block's input when it is a non-empty object, re-encoded as `JSON.stringify` would.
pub(crate) fn whole_input(raw: Option<&RawValue>) -> Option<String> {
    let raw = raw?.get();
    let fields: Map<String, Value> = serde_json::from_str(raw).ok()?;
    if fields.is_empty() {
        return None;
    }
    js_stringify(raw).ok()
}
