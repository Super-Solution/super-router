//! The Gemini adapter against fake upstreams: where the request goes and how it is authorised, refusals before
//! anything is sent, failure classification and the refusal to follow redirects. Request bodies and streams are in
//! `tools.rs` (the conformance file).

use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::Response;
use axum::routing::post;
use axum::Router;
use futures_util::StreamExt;
use serde_json::json;
use super_router_contracts::{Invocation, Message, Offering, Provider, ProviderError, ProviderEvent, ToolCall};
use super_router_provider_gemini::Gemini;

fn invocation() -> Invocation {
    serde_json::from_value(json!({
        "service": "s", "customer_id": "c", "payer_mode": "developer",
        "input": {"messages": [{"role": "user", "content": "hi"}]},
        "limits": {"max_output_tokens": 16, "timeout_ms": 5000},
    }))
    .unwrap()
}

fn offering(model: &str) -> Offering {
    serde_json::from_value(json!({
        "id": "a", "provider": "gemini", "model": "m", "upstream_model": model, "region": "test", "labels": [],
        "enabled": true, "source": "byok", "max_input_tokens": 100, "max_output_tokens": 100,
        "price": {"asset": "USD", "input": "1", "cached": "1", "cache_write": "1", "output": "1", "per": 1},
    }))
    .unwrap()
}

async fn serve(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

const FINAL: &str = r#"data: {"candidates":[{"content":{"role":"model","parts":[{"text":"ok"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":1,"candidatesTokenCount":1,"totalTokenCount":2},"responseId":"r"}

"#;

fn sse(body: &str) -> Response {
    Response::builder().header("content-type", "text/event-stream").body(Body::from(body.to_string())).unwrap()
}

async fn collect(provider: &Gemini, input: &Invocation, model: &str) -> (Vec<ProviderEvent>, Option<ProviderError>) {
    let mut stream = provider.invoke(input, &offering(model));
    let mut events = Vec::new();
    while let Some(item) = stream.next().await {
        match item {
            Ok(e) => events.push(e),
            Err(e) => {
                assert!(stream.next().await.is_none(), "the stream ends after an error");
                return (events, Some(e));
            }
        }
    }
    (events, None)
}

fn shape(e: &ProviderError) -> (bool, bool, &str) {
    (e.definitive, e.retryable, e.code.as_str())
}

type Seen = Arc<Mutex<Option<(Uri, HeaderMap)>>>;

async fn recording(seen: Seen) -> String {
    serve(Router::new().fallback(move |uri: Uri, headers: HeaderMap| async move {
        *seen.lock().unwrap() = Some((uri, headers));
        sse(FINAL)
    }))
    .await
}

#[tokio::test]
async fn the_gemini_api_gets_the_key_header_and_the_v1beta_path() {
    let seen: Seen = Arc::default();
    let base = recording(seen.clone()).await;
    let provider = Gemini::builder("g-key").base_url(&base).build().unwrap();
    let (events, error) = collect(&provider, &invocation(), "gemini-test").await;
    assert_eq!(error, None);
    assert!(matches!(events.last(), Some(ProviderEvent::Done { completion: Some(c) }) if c == "succeeded"));
    let (uri, headers) = seen.lock().unwrap().take().unwrap();
    assert_eq!(uri.path_and_query().unwrap().as_str(), "/v1beta/models/gemini-test:streamGenerateContent?alt=sse");
    assert_eq!(headers["x-goog-api-key"], "g-key");
    assert!(headers.get("authorization").is_none());
    assert_eq!(headers["content-type"], "application/json");
}

#[tokio::test]
async fn vertex_gets_a_bearer_token_and_its_own_path() {
    let seen: Seen = Arc::default();
    let base = recording(seen.clone()).await;
    let provider = Gemini::builder("ya29.token")
        .base_url(format!("{base}/"))
        .path_prefix("/v1/projects/p/locations/us-central1/publishers/google/")
        .bearer_auth()
        .build()
        .unwrap();
    let (_, error) = collect(&provider, &invocation(), "models/gemini-test").await;
    assert_eq!(error, None);
    let (uri, headers) = seen.lock().unwrap().take().unwrap();
    assert_eq!(
        uri.path_and_query().unwrap().as_str(),
        "/v1/projects/p/locations/us-central1/publishers/google/models/gemini-test:streamGenerateContent?alt=sse"
    );
    assert_eq!(headers["authorization"], "Bearer ya29.token");
    assert!(headers.get("x-goog-api-key").is_none());
}

#[tokio::test]
async fn a_model_name_that_could_change_the_path_is_refused_before_anything_is_sent() {
    let seen: Seen = Arc::default();
    let base = recording(seen.clone()).await;
    let provider = Gemini::builder("k").base_url(&base).build().unwrap();
    for model in ["", "a/b", "a?x=1", "a#b", "../x", "a b", "models/", "gem\u{e9}ni"] {
        let (events, error) = collect(&provider, &invocation(), model).await;
        assert!(events.is_empty());
        assert_eq!(shape(&error.unwrap_or_else(|| panic!("{model:?} was accepted"))), (true, false, "request_invalid"), "{model:?}");
    }
    assert!(seen.lock().unwrap().is_none(), "nothing was dispatched");
}

#[tokio::test]
async fn requests_the_upstream_would_reject_never_leave_the_process() {
    let seen: Seen = Arc::default();
    let base = recording(seen.clone()).await;
    let provider = Gemini::builder("k").base_url(&base).build().unwrap();
    let with = |messages: Vec<Message>| {
        let mut input = invocation();
        input.input.messages = messages;
        input
    };
    let call = |arguments: &str| ToolCall { id: "c1".into(), name: "lookup".into(), arguments: arguments.into() };
    let calling = |arguments: &str| Message { role: "assistant".into(), tool_calls: Some(vec![call(arguments)]), ..Message::default() };
    let result = |id: &str| Message { role: "tool".into(), tool_call_id: id.into(), content: "x".into(), ..Message::default() };
    let image = |url: &str| Message {
        role: "user".into(),
        parts: Some(vec![super_router_contracts::ContentPart::Image { url: url.into(), detail: None }]),
        ..Message::default()
    };
    let cases: Vec<(&str, Invocation, &str)> = vec![
        ("arguments that are not an object", with(vec![Message::text("user", "hi"), calling("[1]"), result("c1")]), "request_invalid"),
        ("arguments that are not JSON", with(vec![Message::text("user", "hi"), calling("{"), result("c1")]), "request_invalid"),
        ("a result for a call never made", with(vec![Message::text("user", "hi"), result("nope")]), "request_invalid"),
        ("an image that is a link", with(vec![image("https://e.example/a.png")]), "request_invalid"),
        ("nothing but a system message", with(vec![Message::text("system", "Be brief")]), "messages_required"),
    ];
    for (name, input, code) in cases {
        let (events, error) = collect(&provider, &input, "gemini-test").await;
        assert!(events.is_empty(), "{name}");
        assert_eq!(shape(&error.unwrap_or_else(|| panic!("{name}: no error"))), (true, false, code), "{name}");
    }
    // No arguments at all is an empty object, not a refusal.
    let (_, error) = collect(&provider, &with(vec![Message::text("user", "hi"), calling(""), result("c1")]), "gemini-test").await;
    assert_eq!(error, None);
}

#[tokio::test]
async fn failures_are_classified() {
    let status = |code: u16| move || Response::builder().status(code).body(Body::empty()).unwrap();
    let mut task = invocation();
    task.input.task = Some(serde_json::from_value(json!({"kind": "image", "prompt": "x"})).unwrap());
    type Case = (&'static str, u16, &'static str, Invocation, (bool, bool, &'static str));
    let cases: Vec<Case> = vec![
        ("rate limited", 429, "k", invocation(), (true, true, "upstream_http_429")),
        ("bad request", 400, "k", invocation(), (true, false, "upstream_http_400")),
        ("unauthorized", 401, "k", invocation(), (true, false, "upstream_http_401")),
        ("forbidden", 403, "k", invocation(), (true, false, "upstream_http_403")),
        ("not found", 404, "k", invocation(), (true, false, "upstream_http_404")),
        ("server error is unknown", 500, "k", invocation(), (false, false, "upstream_http_500")),
        ("unavailable is unknown", 503, "k", invocation(), (false, false, "upstream_http_503")),
        ("empty 200 is interrupted", 200, "k", invocation(), (false, false, "stream_interrupted")),
        ("no credentials", 200, "", invocation(), (true, false, "credentials_missing")),
        ("a key that cannot be a header", 200, "bad\nkey", invocation(), (true, false, "request_invalid")),
        ("tasks unsupported", 200, "k", task, (true, false, "unsupported_task")),
    ];
    for (name, code, key, input, want) in cases {
        let respond = status(code);
        let base = serve(Router::new().fallback(move || async move { respond() })).await;
        let provider = Gemini::builder(key).base_url(&base).build().unwrap();
        let (events, error) = collect(&provider, &input, "gemini-test").await;
        assert!(events.is_empty(), "{name}");
        assert_eq!(shape(&error.unwrap_or_else(|| panic!("{name}: no error"))), want, "{name}");
    }
}

#[tokio::test]
async fn a_redirect_is_refused_and_never_followed() {
    let hit = Arc::new(Mutex::new(false));
    let flag = hit.clone();
    let target = serve(Router::new().fallback(move || async move {
        *flag.lock().unwrap() = true;
        sse(FINAL)
    }))
    .await;
    let location = format!("{target}/elsewhere");
    let base = serve(Router::new().route(
        "/v1beta/models/gemini-test:streamGenerateContent",
        post(move || async move {
            Response::builder().status(StatusCode::TEMPORARY_REDIRECT).header("location", location).body(Body::empty()).unwrap()
        }),
    ))
    .await;
    let provider = Gemini::builder("k").base_url(&base).build().unwrap();
    let (_, error) = collect(&provider, &invocation(), "gemini-test").await;
    assert_eq!(shape(&error.unwrap()), (false, false, "upstream_http_307"));
    assert!(!*hit.lock().unwrap(), "the redirect target was never contacted");
}

#[tokio::test]
async fn the_key_never_appears_in_debug_output() {
    let provider = Gemini::builder("super-secret").build().unwrap();
    let text = format!("{provider:?}");
    assert!(!text.contains("super-secret"), "{text}");
}
