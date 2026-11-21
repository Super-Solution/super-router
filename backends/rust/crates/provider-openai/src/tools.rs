//! Tool calling and image parts on the Responses API: how messages, tools and the tool choice are written, and
//! how the numbers of a streamed event are read.

use serde_json::{json, Map, Value};
use super_router_contracts::{ContentPart, Input, Message, ToolChoice};

/// Converts messages to the Responses API `input` list. Plain text messages pass through unchanged; tool calls
/// become `function_call` items and tool results `function_call_output` items; image parts become `input_image`.
pub fn input(messages: &[Message]) -> Vec<Value> {
    let mut out = Vec::with_capacity(messages.len());
    for m in messages {
        if m.role == "tool" {
            out.push(json!({ "type": "function_call_output", "call_id": m.tool_call_id, "output": m.content }));
        } else if let Some(calls) = &m.tool_calls {
            if !m.content.is_empty() {
                out.push(json!({ "role": "assistant", "content": m.content }));
            }
            for c in calls {
                out.push(json!({ "type": "function_call", "call_id": c.id, "name": c.name, "arguments": c.arguments }));
            }
        } else if let Some(parts) = &m.parts {
            let content: Vec<Value> = parts
                .iter()
                .map(|p| match p {
                    ContentPart::Text { text } => json!({ "type": "input_text", "text": text }),
                    ContentPart::Image { url, detail } => {
                        let mut image = json!({ "type": "input_image", "image_url": url });
                        if let Some(d) = detail {
                            image["detail"] = json!(d);
                        }
                        image
                    }
                })
                .collect();
            out.push(json!({ "role": m.role, "content": content }));
        } else {
            out.push(json!({ "role": m.role, "content": m.content }));
        }
    }
    out
}

/// Adds `tools` and `tool_choice` to a request body, and nothing when the request declares no tools.
pub(crate) fn tool_fields(body: &mut Map<String, Value>, input: &Input) {
    if let Some(tools) = &input.tools {
        let tools: Vec<Value> = tools
            .iter()
            .map(|t| {
                let mut tool = json!({ "type": "function", "name": t.name, "parameters": t.input_schema, "strict": false });
                if let Some(d) = &t.description {
                    tool["description"] = json!(d);
                }
                tool
            })
            .collect();
        body.insert("tools".into(), Value::Array(tools));
    }
    match &input.tool_choice {
        None => {}
        Some(ToolChoice::Mode(mode)) => {
            body.insert("tool_choice".into(), json!(mode));
        }
        Some(ToolChoice::Name(name)) => {
            body.insert("tool_choice".into(), json!({ "type": "function", "name": name }));
        }
    }
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
