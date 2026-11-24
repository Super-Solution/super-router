//! Runs the provider sections of `conformance/tools.v1.json` (and files of the same shape) against a [`Provider`]: the exact upstream request
//! body built from an invocation, and the events produced from a recorded upstream stream.

use std::path::Path;
use std::sync::{Arc, Mutex};

use axum::body::{Body, Bytes};
use axum::http::Request;
use axum::response::Response;
use axum::Router;
use futures_util::StreamExt;
use serde_json::value::RawValue;
use serde_json::{json, Number, Value};
use super_router_contracts::{parse_invocation, Offering, Provider, ProviderError, ProviderEvent};

fn read(file: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../conformance").join(file);
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {file}: {e}"))
}

/// The recorded frames of every stream case, as written in the file: key order matters to a tool call's input.
fn raw_frames(text: &str, section: &str) -> Vec<Vec<String>> {
    #[derive(serde::Deserialize)]
    struct Stream {
        frames: Vec<Box<RawValue>>,
    }
    #[derive(serde::Deserialize)]
    struct Section {
        streams: Vec<Stream>,
    }
    let mut file: std::collections::HashMap<String, Box<RawValue>> = serde_json::from_str(text).expect("parse conformance file");
    let section: Section = serde_json::from_str(file.remove(section).expect("section").get()).expect("section");
    section.streams.into_iter().map(|s| s.frames.into_iter().map(|f| f.get().to_string()).collect()).collect()
}

/// Generic JSON compares every number as a double.
fn normalise(v: Value) -> Value {
    match v {
        Value::Number(n) => Value::Number(Number::from_f64(n.as_f64().unwrap()).unwrap()),
        Value::Array(a) => Value::Array(a.into_iter().map(normalise).collect()),
        Value::Object(m) => Value::Object(m.into_iter().map(|(k, v)| (k, normalise(v))).collect()),
        other => other,
    }
}

/// The event as the reference writes it.
fn wire(e: &ProviderEvent) -> Value {
    match e {
        ProviderEvent::Delta { text } => json!({ "type": "delta", "text": text }),
        ProviderEvent::ToolCall { index, id, name, arguments_delta } => {
            let mut out = json!({ "type": "tool_call", "index": index, "arguments_delta": arguments_delta });
            if let Some(id) = id {
                out["id"] = json!(id);
                out["name"] = json!(name);
            }
            out
        }
        ProviderEvent::Usage { usage, upstream_id } => {
            let mut out = json!({ "type": "usage", "usage": usage });
            if !upstream_id.is_empty() {
                out["upstream_id"] = json!(upstream_id);
            }
            out
        }
        ProviderEvent::Done { completion } => {
            let mut out = json!({ "type": "done" });
            if let Some(c) = completion {
                out["completion"] = json!(c);
            }
            out
        }
    }
}

/// Removes the whitespace between JSON tokens, keeping everything else byte for byte.
fn compact(raw: &str) -> String {
    let (mut out, mut in_string, mut escaped) = (String::with_capacity(raw.len()), false, false);
    for c in raw.chars() {
        if in_string {
            out.push(c);
            match (escaped, c) {
                (true, _) => escaped = false,
                (false, '\\') => escaped = true,
                (false, '"') => in_string = false,
                _ => {}
            }
        } else if c == '"' {
            in_string = true;
            out.push(c);
        } else if !c.is_ascii_whitespace() {
            out.push(c);
        }
    }
    out
}

/// Serves `respond` for every request and records the last request body.
async fn upstream(respond: Arc<dyn Fn() -> Response + Send + Sync>, seen: Arc<Mutex<Option<Bytes>>>) -> String {
    let app = Router::new().fallback(move |req: Request<Body>| {
        let (respond, seen) = (respond.clone(), seen.clone());
        async move {
            let body = axum::body::to_bytes(req.into_body(), usize::MAX).await.unwrap_or_default();
            *seen.lock().unwrap() = Some(body);
            respond()
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("address");
    tokio::spawn(async move { axum::serve(listener, app).await.expect("serve") });
    format!("http://{addr}")
}

async fn invoke(
    make: &dyn Fn(&str) -> Arc<dyn Provider>,
    offering: &Offering,
    invocation: &Value,
    respond: Arc<dyn Fn() -> Response + Send + Sync>,
) -> (Vec<Value>, Option<ProviderError>, Option<Bytes>) {
    let input = parse_invocation(&serde_json::to_vec(invocation).unwrap()).expect("invocation");
    let seen = Arc::new(Mutex::new(None));
    let base = upstream(respond, seen.clone()).await;
    let provider = make(&base);
    let mut stream = provider.invoke(&input, offering);
    let mut events = Vec::new();
    let mut error = None;
    while let Some(item) = stream.next().await {
        match item {
            Ok(e) => events.push(wire(&e)),
            Err(e) => {
                error = Some(e);
                break;
            }
        }
    }
    let body = seen.lock().unwrap().take();
    (events, error, body)
}

/// Checks a provider against the named section (`openai` or `anthropic`) and returns every difference found.
/// `make` builds the provider pointed at a test server's base URL.
pub async fn run_tool_cases(section: &str, make: &dyn Fn(&str) -> Arc<dyn Provider>) -> Vec<String> {
    run_tool_cases_in("tools.v1.json", section, make).await
}

/// Like [`run_tool_cases`] for a file of the same shape in `backends/rust/conformance`. Providers the TypeScript
/// reference does not have (Gemini) keep their cases in a file of their own, because `tools.v1.json` is generated
/// from the reference.
pub async fn run_tool_cases_in(file: &str, section: &str, make: &dyn Fn(&str) -> Arc<dyn Provider>) -> Vec<String> {
    let text = read(file);
    let data: Value = serde_json::from_str(&text).expect("parse conformance file");
    let recorded = raw_frames(&text, section);
    let offering: Offering = serde_json::from_value(data["offering"].clone()).expect("offering");
    let requests = data[section]["requests"].as_array().expect("requests");
    let streams = data[section]["streams"].as_array().expect("streams");
    assert!(requests.len() >= 5 && streams.len() >= 5, "{section}: only {} requests and {} streams", requests.len(), streams.len());
    let mut failures = Vec::new();
    for c in requests {
        let name = c["name"].as_str().unwrap_or_default();
        let empty: Arc<dyn Fn() -> Response + Send + Sync> =
            Arc::new(|| Response::builder().header("content-type", "text/event-stream").body(Body::empty()).unwrap());
        let (_, _, body) = invoke(make, &offering, &c["invocation"], empty).await;
        let have = body.and_then(|b| serde_json::from_slice::<Value>(&b).ok()).map(normalise);
        if have.as_ref() != Some(&normalise(c["body"].clone())) {
            failures.push(format!("request {name}:\n have {have:?}\n want {}", c["body"]));
        }
    }
    for (c, recorded) in streams.iter().zip(recorded) {
        let name = c["name"].as_str().unwrap_or_default();
        // One line per frame: the file is pretty-printed, so the whitespace goes (outside strings only).
        let frames: String = recorded.iter().map(|f| format!("data: {}\n\n", compact(f))).collect();
        let respond: Arc<dyn Fn() -> Response + Send + Sync> =
            Arc::new(move || Response::builder().header("content-type", "text/event-stream").body(Body::from(frames.clone())).unwrap());
        let (events, error, _) = invoke(make, &offering, &c["invocation"], respond).await;
        let want_error = c.get("error").and_then(Value::as_str);
        if error.as_ref().map(|e| e.code.as_str()) != want_error {
            failures.push(format!("stream {name}: error {:?}, want {want_error:?}", error.map(|e| e.code)));
        }
        let have = normalise(Value::Array(events));
        if have != normalise(c["events"].clone()) {
            failures.push(format!("stream {name}:\n have {have}\n want {}", c["events"]));
        }
    }
    failures
}
