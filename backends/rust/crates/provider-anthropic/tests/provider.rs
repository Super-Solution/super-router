//! The Anthropic adapter against fake upstreams: request shape, usage normalisation, failure classification, the
//! refusal to follow redirects, bounded frames and abandoning the upstream call.

use std::convert::Infallible;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::post;
use axum::Router;
use bytes::Bytes;
use futures_util::StreamExt;
use serde_json::{json, Value};
use super_router_contracts::{Invocation, Meters, Offering, Provider, ProviderError, ProviderEvent};
use super_router_provider_anthropic::Anthropic;

fn invocation() -> Invocation {
    serde_json::from_value(json!({
        "service": "s", "customer_id": "c", "payer_mode": "developer",
        "input": {"messages": [{"role": "system", "content": "be brief"}, {"role": "user", "content": "hi"}]},
        "limits": {"max_output_tokens": 16, "timeout_ms": 5000},
    }))
    .unwrap()
}

fn offering() -> Offering {
    serde_json::from_value(json!({
        "id": "a", "provider": "anthropic", "model": "m", "upstream_model": "claude-test", "region": "test", "labels": [],
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

fn frame(data: &str) -> String {
    format!("event: x\ndata: {data}\n\n")
}

fn sse(body: String) -> Response {
    Response::builder().header("content-type", "text/event-stream").body(Body::from(body)).unwrap()
}

/// A fake upstream that answers every POST with the same canned response.
async fn upstream(respond: impl Fn() -> Response + Clone + Send + Sync + 'static) -> String {
    serve(Router::new().route("/v1/messages", post(move || async move { respond() }))).await
}

/// Everything the provider produced: the events before the first error, and that error.
async fn collect(provider: &Anthropic, input: &Invocation) -> (Vec<ProviderEvent>, Option<ProviderError>) {
    let mut stream = provider.invoke(input, &offering());
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

fn provider(base: &str, key: &str) -> Anthropic {
    Anthropic::builder(key).base_url(base).build().unwrap()
}

fn shape(e: &ProviderError) -> (bool, bool, &str) {
    (e.definitive, e.retryable, e.code.as_str())
}

const START: &str = r#"{"type":"message_start","message":{"id":"m","usage":{"input_tokens":1,"output_tokens":1}}}"#;
const STOP: &str = r#"{"type":"message_stop"}"#;

fn status(code: u16) -> impl Fn() -> Response + Clone + Send + Sync + 'static {
    move || Response::builder().status(StatusCode::from_u16(code).unwrap()).body(Body::empty()).unwrap()
}

fn canned(frames: &[&str]) -> impl Fn() -> Response + Clone + Send + Sync + 'static {
    let body: String = frames.iter().map(|f| frame(f)).collect();
    move || sse(body.clone())
}

#[tokio::test]
async fn streams_text_and_merges_usage() {
    let seen: Arc<Mutex<Option<(HeaderMap, Value)>>> = Arc::default();
    let sink = seen.clone();
    let frames = [
        frame(r#"{"type":"message_start","message":{"id":"msg_1","usage":{"input_tokens":50,"cache_read_input_tokens":20,"cache_creation_input_tokens":5,"output_tokens":1,"cache_creation":{"ephemeral_5m_input_tokens":5}}}}"#),
        frame(r#"{"type":"content_block_delta","delta":{"type":"text_delta","text":"Hi"}}"#),
        frame(r#"{"type":"content_block_delta","delta":{"type":"thinking_delta","thinking":"hm"}}"#),
        frame(r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":9,"server_tool_use":{"web_search_requests":0}}}"#),
        frame(STOP),
    ]
    .concat();
    let app = Router::new().route(
        "/v1/messages",
        post(move |headers: HeaderMap, body: Bytes| async move {
            *sink.lock().unwrap() = Some((headers, serde_json::from_slice(&body).unwrap()));
            sse(frames)
        }),
    );
    let base = serve(app).await;
    let (events, error) = collect(&provider(&base, "key"), &invocation()).await;
    assert_eq!(error, None);
    assert_eq!(
        events,
        vec![
            ProviderEvent::Delta { text: "Hi".into() },
            ProviderEvent::Usage {
                usage: Meters { input: 50, cached: 20, cache_write: 5, output: 9, ..Meters::default() },
                upstream_id: "msg_1".into()
            },
            ProviderEvent::Done { completion: Some("succeeded".into()) },
        ]
    );
    let (headers, body) = seen.lock().unwrap().take().expect("the upstream was called");
    assert_eq!(headers["x-api-key"], "key");
    assert_eq!(headers["anthropic-version"], "2023-06-01");
    assert_eq!(headers["content-type"], "application/json");
    assert!(headers.get("authorization").is_none());
    assert_eq!(
        body,
        json!({"model": "claude-test", "max_tokens": 16, "system": "be brief", "messages": [{"role": "user", "content": "hi"}], "stream": true})
    );
}

#[tokio::test]
async fn system_messages_are_joined_and_omitted_when_there_are_none() {
    let seen: Arc<Mutex<Vec<Value>>> = Arc::default();
    let sink = seen.clone();
    let done = frame(START) + &frame(STOP);
    let app = Router::new().route(
        "/v1/messages",
        post(move |body: Bytes| {
            let (sink, done) = (sink.clone(), done.clone());
            async move {
                sink.lock().unwrap().push(serde_json::from_slice(&body).unwrap());
                sse(done)
            }
        }),
    );
    let base = serve(app).await;
    let mut input = invocation();
    input.input.messages = serde_json::from_value(json!([
        {"role": "system", "content": "one"}, {"role": "user", "content": "q"}, {"role": "system", "content": "two"},
        {"role": "assistant", "content": "a"}, {"role": "user", "content": "q2"},
    ]))
    .unwrap();
    assert_eq!(collect(&provider(&base, "k"), &input).await.1, None);
    input.input.messages = serde_json::from_value(json!([{"role": "user", "content": "only"}])).unwrap();
    assert_eq!(collect(&provider(&base, "k"), &input).await.1, None);
    let seen = seen.lock().unwrap();
    assert_eq!(seen[0]["system"], "one\ntwo");
    assert_eq!(
        seen[0]["messages"],
        json!([{"role": "user", "content": "q"}, {"role": "assistant", "content": "a"}, {"role": "user", "content": "q2"}])
    );
    assert!(seen[1].get("system").is_none());
}

#[tokio::test]
async fn max_tokens_is_partial() {
    let respond = canned(&[START, r#"{"type":"message_delta","delta":{"stop_reason":"max_tokens"},"usage":{"output_tokens":16}}"#, STOP]);
    let base = upstream(respond).await;
    let (events, error) = collect(&provider(&base, "k"), &invocation()).await;
    assert_eq!(error, None);
    assert_eq!(events.last(), Some(&ProviderEvent::Done { completion: Some("partial".into()) }));
}

#[tokio::test]
async fn a_delta_may_complete_the_counters_that_the_start_lacked() {
    let respond = canned(&[
        r#"{"type":"message_start","message":{"id":"m","usage":{"input_tokens":7}}}"#,
        r#"{"type":"message_delta","delta":{},"usage":{"output_tokens":4}}"#,
        STOP,
    ]);
    let base = upstream(respond).await;
    let (events, error) = collect(&provider(&base, "k"), &invocation()).await;
    assert_eq!(error, None);
    assert_eq!(events[0], ProviderEvent::Usage { usage: Meters { input: 7, output: 4, ..Meters::default() }, upstream_id: "m".into() });
}

#[tokio::test]
async fn a_delta_overwrites_rather_than_adds() {
    let respond = canned(&[
        r#"{"type":"message_start","message":{"id":"m","usage":{"input_tokens":7,"output_tokens":1,"cache_read_input_tokens":3}}}"#,
        r#"{"type":"message_delta","delta":{},"usage":{"output_tokens":4,"cache_read_input_tokens":6}}"#,
        r#"{"type":"message_delta","delta":{},"usage":{"output_tokens":9}}"#,
        STOP,
    ]);
    let base = upstream(respond).await;
    let (events, _) = collect(&provider(&base, "k"), &invocation()).await;
    assert_eq!(
        events[0],
        ProviderEvent::Usage { usage: Meters { input: 7, cached: 6, output: 9, ..Meters::default() }, upstream_id: "m".into() }
    );
}

/// A named failure: how the fake upstream answers, the key, the invocation and the expected (definitive, retryable, code).
type Case<'a> = (&'a str, Box<dyn Fn() -> Response + Send + Sync>, &'a str, Invocation, (bool, bool, &'a str));

#[tokio::test]
async fn failures_are_classified() {
    let mut task = invocation();
    task.input.task = Some(serde_json::from_value(json!({"kind": "image", "prompt": "x"})).unwrap());
    let mut only_system = invocation();
    only_system.input.messages.truncate(1);
    let ok = invocation();
    let cases: Vec<Case> = vec![
        ("rate limited", Box::new(status(429)), "k", ok.clone(), (true, true, "rate_limited")),
        ("bad request", Box::new(status(400)), "k", ok.clone(), (true, false, "upstream_http_400")),
        ("unauthorized", Box::new(status(401)), "k", ok.clone(), (true, true, "credentials_rejected")),
        ("forbidden", Box::new(status(403)), "k", ok.clone(), (true, true, "credentials_rejected")),
        ("not found", Box::new(status(404)), "k", ok.clone(), (true, true, "model_unavailable")),
        ("unprocessable", Box::new(status(422)), "k", ok.clone(), (true, false, "upstream_http_422")),
        ("overloaded is unknown", Box::new(status(529)), "k", ok.clone(), (false, false, "upstream_http_529")),
        ("server error is unknown", Box::new(status(500)), "k", ok.clone(), (false, false, "upstream_http_500")),
        ("no credentials", Box::new(status(200)), "", ok.clone(), (true, false, "credentials_missing")),
        ("a key that cannot be a header", Box::new(status(200)), "bad\nkey", ok.clone(), (true, false, "request_invalid")),
        ("tasks unsupported", Box::new(status(200)), "k", task, (true, false, "unsupported_task")),
        ("only system messages", Box::new(status(200)), "k", only_system, (true, false, "messages_required")),
        ("truncated stream", Box::new(canned(&[START])), "k", ok.clone(), (false, false, "stream_interrupted")),
        ("empty 200", Box::new(status(200)), "k", ok.clone(), (false, false, "stream_interrupted")),
        ("stop without start", Box::new(canned(&[STOP])), "k", ok.clone(), (false, false, "usage_unknown")),
        (
            "start without a usage object",
            Box::new(canned(&[r#"{"type":"message_start","message":{"id":"m"}}"#, STOP])),
            "k",
            ok.clone(),
            (false, false, "usage_unknown"),
        ),
        (
            "start without counters is not free",
            Box::new(canned(&[r#"{"type":"message_start","message":{"id":"m","usage":{}}}"#, STOP])),
            "k",
            ok.clone(),
            (false, false, "invalid_usage"),
        ),
        (
            "start without output",
            Box::new(canned(&[r#"{"type":"message_start","message":{"id":"m","usage":{"input_tokens":3}}}"#, STOP])),
            "k",
            ok.clone(),
            (false, false, "invalid_usage"),
        ),
        (
            "start without input",
            Box::new(canned(&[r#"{"type":"message_start","message":{"id":"m","usage":{"output_tokens":3}}}"#, STOP])),
            "k",
            ok.clone(),
            (false, false, "invalid_usage"),
        ),
        (
            "string counter in a delta",
            Box::new(canned(&[START, r#"{"type":"message_delta","delta":{},"usage":{"output_tokens":"9"}}"#])),
            "k",
            ok.clone(),
            (false, false, "invalid_usage"),
        ),
        (
            "fractional counter in a delta",
            Box::new(canned(&[START, r#"{"type":"message_delta","delta":{},"usage":{"output_tokens":9.5}}"#])),
            "k",
            ok.clone(),
            (false, false, "invalid_usage"),
        ),
        (
            "null counter in a delta",
            Box::new(canned(&[START, r#"{"type":"message_delta","delta":{},"usage":{"output_tokens":null}}"#, STOP])),
            "k",
            ok.clone(),
            (false, false, "invalid_usage"),
        ),
        (
            "null cache counter in a delta",
            Box::new(canned(&[START, r#"{"type":"message_delta","delta":{},"usage":{"cache_read_input_tokens":null}}"#, STOP])),
            "k",
            ok.clone(),
            (false, false, "invalid_usage"),
        ),
        (
            "negative counter in a delta",
            Box::new(canned(&[START, r#"{"type":"message_delta","delta":{},"usage":{"output_tokens":-1}}"#, STOP])),
            "k",
            ok.clone(),
            (false, false, "invalid_usage"),
        ),
        (
            "negative cache counter",
            Box::new(canned(&[
                r#"{"type":"message_start","message":{"id":"m","usage":{"input_tokens":1,"output_tokens":1,"cache_read_input_tokens":-4}}}"#,
                STOP,
            ])),
            "k",
            ok.clone(),
            (false, false, "invalid_usage"),
        ),
        (
            "counter beyond a safe integer",
            Box::new(canned(&[
                r#"{"type":"message_start","message":{"id":"m","usage":{"input_tokens":9007199254740993,"output_tokens":1}}}"#,
                STOP,
            ])),
            "k",
            ok.clone(),
            (false, false, "invalid_usage"),
        ),
        (
            "fractional counter at the start",
            Box::new(canned(&[r#"{"type":"message_start","message":{"id":"m","usage":{"input_tokens":1.5,"output_tokens":1}}}"#, STOP])),
            "k",
            ok.clone(),
            (false, false, "invalid_upstream_event"),
        ),
        (
            "usage that is not an object",
            Box::new(canned(&[START, r#"{"type":"message_delta","delta":{},"usage":5}"#])),
            "k",
            ok.clone(),
            (false, false, "invalid_upstream_event"),
        ),
        ("error event", Box::new(canned(&[START, r#"{"type":"error"}"#])), "k", ok.clone(), (false, false, "upstream_stream_error")),
        ("bad event json", Box::new(canned(&["{"])), "k", ok.clone(), (false, false, "invalid_upstream_event")),
        ("event that is not an object", Box::new(canned(&["[]"])), "k", ok.clone(), (false, false, "invalid_upstream_event")),
    ];
    for (name, respond, key, input, want) in cases {
        let respond: Arc<dyn Fn() -> Response + Send + Sync> = Arc::from(respond);
        let base = serve(Router::new().route("/v1/messages", post(move || async move { respond() }))).await;
        let (_, error) = collect(&provider(&base, key), &input).await;
        let error = error.unwrap_or_else(|| panic!("{name}: no error"));
        assert_eq!(shape(&error), want, "{name}");
    }
}

#[tokio::test]
async fn nothing_is_dispatched_for_a_definitive_local_refusal() {
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    let app = Router::new().fallback(move || {
        let counter = counter.clone();
        async move {
            counter.fetch_add(1, Ordering::SeqCst);
            StatusCode::OK
        }
    });
    let base = serve(app).await;
    let mut task = invocation();
    task.input.task = Some(serde_json::from_value(json!({"kind": "image", "prompt": "x"})).unwrap());
    let mut only_system = invocation();
    only_system.input.messages.truncate(1);
    for (input, key) in [(task, "k"), (only_system, "k"), (invocation(), ""), (invocation(), "bad\nkey")] {
        assert!(collect(&provider(&base, key), &input).await.1.unwrap().definitive);
    }
    assert_eq!(hits.load(Ordering::SeqCst), 0, "a definitive refusal means the upstream was never called");
}

#[tokio::test]
async fn a_failure_after_output_still_reports_the_output_first() {
    let respond = canned(&[START, r#"{"type":"content_block_delta","delta":{"type":"text_delta","text":"a"}}"#, r#"{"type":"error"}"#]);
    let base = upstream(respond).await;
    let (events, error) = collect(&provider(&base, "k"), &invocation()).await;
    assert_eq!(events, vec![ProviderEvent::Delta { text: "a".into() }]);
    assert_eq!(shape(&error.unwrap()), (false, false, "upstream_stream_error"));
}

#[tokio::test]
async fn nothing_is_read_after_the_terminal_event() {
    let base = upstream(canned(&[START, STOP, "not json at all"])).await;
    assert_eq!(collect(&provider(&base, "k"), &invocation()).await.1, None);
}

#[tokio::test]
async fn a_redirect_is_refused_and_never_followed() {
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    let target = serve(Router::new().fallback(move || {
        let counter = counter.clone();
        async move {
            counter.fetch_add(1, Ordering::SeqCst);
            StatusCode::OK
        }
    }))
    .await;
    for code in [301, 302, 307, 308] {
        let location = format!("{target}/stolen");
        let base = upstream(move || Response::builder().status(code).header("location", &location).body(Body::empty()).unwrap()).await;
        let (_, error) = collect(&provider(&base, "secret-key"), &invocation()).await;
        assert_eq!(shape(&error.unwrap()), (false, false, &*format!("upstream_http_{code}")));
    }
    assert_eq!(hits.load(Ordering::SeqCst), 0, "the redirect target must never be contacted, with or without the key");
}

#[tokio::test]
async fn a_client_builder_cannot_turn_redirects_back_on() {
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    let target = serve(Router::new().fallback(move || {
        let counter = counter.clone();
        async move {
            counter.fetch_add(1, Ordering::SeqCst);
            StatusCode::OK
        }
    }))
    .await;
    let location = format!("{target}/x");
    let base = upstream(move || Response::builder().status(302).header("location", &location).body(Body::empty()).unwrap()).await;
    let provider = Anthropic::builder("k")
        .base_url(&base)
        .client_builder(reqwest::Client::builder().redirect(reqwest::redirect::Policy::limited(10)))
        .build()
        .unwrap();
    let (_, error) = collect(&provider, &invocation()).await;
    assert_eq!(shape(&error.unwrap()), (false, false, "upstream_http_302"));
    assert_eq!(hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_line_that_never_ends_is_refused_as_an_oversized_frame() {
    let app = Router::new().route(
        "/v1/messages",
        post(|| async {
            let endless = futures_util::stream::repeat_with(|| Ok::<_, Infallible>(Bytes::from(vec![b'a'; 16 * 1024])));
            Response::builder().header("content-type", "text/event-stream").body(Body::from_stream(endless)).unwrap()
        }),
    );
    let base = serve(app).await;
    let (events, error) =
        tokio::time::timeout(Duration::from_secs(20), collect(&provider(&base, "k"), &invocation())).await.expect("the read is bounded");
    assert!(events.is_empty());
    assert_eq!(shape(&error.unwrap()), (false, false, "stream_frame_too_large"));
}

/// Flips a flag when dropped: the fake upstream's proof that the client went away.
struct OnDrop(Arc<AtomicBool>);

impl Drop for OnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

async fn hanging_upstream(closed: Arc<AtomicBool>, first_frame: Option<String>) -> String {
    let app = Router::new().route(
        "/v1/messages",
        post(move || {
            let closed = closed.clone();
            let first = first_frame.clone();
            async move {
                let body = async_stream::stream! {
                    let _guard = OnDrop(closed);
                    if let Some(frame) = first {
                        yield Ok::<_, Infallible>(Bytes::from(frame));
                    }
                    std::future::pending::<()>().await;
                };
                Response::builder().header("content-type", "text/event-stream").body(Body::from_stream(body)).unwrap()
            }
        }),
    );
    serve(app).await
}

async fn eventually(flag: &AtomicBool) -> bool {
    for _ in 0..100 {
        if flag.load(Ordering::SeqCst) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

const TEXT_A: &str = r#"{"type":"content_block_delta","delta":{"type":"text_delta","text":"a"}}"#;

#[tokio::test]
async fn dropping_the_stream_abandons_the_upstream_call() {
    let closed = Arc::new(AtomicBool::new(false));
    let base = hanging_upstream(closed.clone(), Some(frame(TEXT_A))).await;
    let provider = provider(&base, "k");
    let mut stream = provider.invoke(&invocation(), &offering());
    assert_eq!(stream.next().await.unwrap().unwrap(), ProviderEvent::Delta { text: "a".into() });
    assert!(!closed.load(Ordering::SeqCst), "still streaming");
    drop(stream);
    assert!(eventually(&closed).await, "the upstream connection was left open after the consumer stopped");
}

#[tokio::test]
async fn dropping_the_stream_before_the_response_headers_abandons_the_call() {
    let hit = Arc::new(AtomicBool::new(false));
    let closed = Arc::new(AtomicBool::new(false));
    let (h, c) = (hit.clone(), closed.clone());
    let app = Router::new().route(
        "/v1/messages",
        post(move || {
            let (h, c) = (h.clone(), c.clone());
            async move {
                h.store(true, Ordering::SeqCst);
                let _guard = OnDrop(c);
                std::future::pending::<Response>().await
            }
        }),
    );
    let base = serve(app).await;
    let provider = provider(&base, "k");
    let mut stream = provider.invoke(&invocation(), &offering());
    assert!(tokio::time::timeout(Duration::from_millis(300), stream.next()).await.is_err(), "no answer yet");
    assert!(hit.load(Ordering::SeqCst));
    drop(stream);
    assert!(eventually(&closed).await, "the in-flight request was left open");
}

#[tokio::test]
async fn the_invocation_timeout_bounds_the_whole_call_and_is_unknown() {
    let closed = Arc::new(AtomicBool::new(false));
    let base = hanging_upstream(closed.clone(), Some(frame(TEXT_A))).await;
    let mut input = invocation();
    input.limits.timeout_ms = 300;
    let started = std::time::Instant::now();
    let (events, error) = collect(&provider(&base, "k"), &input).await;
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(events, vec![ProviderEvent::Delta { text: "a".into() }]);
    assert_eq!(shape(&error.unwrap()), (false, false, "stream_interrupted"));
    assert!(eventually(&closed).await);
}

#[tokio::test]
async fn an_unreachable_upstream_is_unknown_not_definitive() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let (_, error) = collect(&provider(&base, "k"), &invocation()).await;
    assert_eq!(shape(&error.unwrap()), (false, false, "transport_unknown"));
}

#[tokio::test]
async fn the_key_is_read_on_every_call_so_it_can_rotate() {
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let sink = seen.clone();
    let body = frame(START) + &frame(STOP);
    let app = Router::new().route(
        "/v1/messages",
        post(move |headers: HeaderMap| {
            let (sink, body) = (sink.clone(), body.clone());
            async move {
                sink.lock().unwrap().push(headers["x-api-key"].to_str().unwrap().to_string());
                sse(body)
            }
        }),
    );
    let base = serve(app).await;
    let current = Arc::new(Mutex::new("one".to_string()));
    let source = current.clone();
    let provider = Anthropic::builder_with_key_source(Arc::new(move || source.lock().unwrap().clone())).base_url(base).build().unwrap();
    assert_eq!(collect(&provider, &invocation()).await.1, None);
    *current.lock().unwrap() = "two".to_string();
    assert_eq!(collect(&provider, &invocation()).await.1, None);
    assert_eq!(*seen.lock().unwrap(), ["one", "two"]);
}

#[tokio::test]
async fn the_key_and_the_response_body_never_appear_in_errors_or_debug_output() {
    let secret = "sk-ant-very-secret-value";
    let base = upstream(|| Response::builder().status(401).body(Body::from("echoed body: sk-ant-very-secret-value")).unwrap()).await;
    let provider = provider(&base, secret);
    let (_, error) = collect(&provider, &invocation()).await;
    let error = error.unwrap();
    for text in [format!("{error:?}"), error.to_string(), format!("{provider:?}")] {
        assert!(!text.contains(secret) && !text.contains("echoed"), "{text}");
    }
}

#[tokio::test]
async fn a_trailing_slash_on_the_base_url_is_harmless() {
    let base = upstream(canned(&[START, STOP])).await;
    let provider = Anthropic::builder("k").base_url(format!("{base}/")).build().unwrap();
    assert_eq!(collect(&provider, &invocation()).await.1, None);
}

#[tokio::test]
async fn a_retry_after_on_a_refusal_is_carried_in_milliseconds() {
    for (header, want) in
        [(Some("12"), Some(12_000)), (Some("1.5"), Some(1_500)), (Some("Wed, 21 Oct 2026 07:28:00 GMT"), None), (None, None)]
    {
        let base = upstream(move || {
            let mut r = Response::builder().status(429);
            if let Some(h) = header {
                r = r.header("retry-after", h);
            }
            r.body(Body::empty()).unwrap()
        })
        .await;
        let (events, error) = collect(&provider(&base, "k"), &invocation()).await;
        let error = error.expect("a refusal");
        assert!(events.is_empty());
        assert_eq!((shape(&error), error.retry_after_ms), ((true, true, "rate_limited"), want), "{header:?}");
    }
}

/// Every response in `conformance/failure.v1.json` (`provider_responses`), served as a real HTTP refusal: the status,
/// the `Retry-After` header and the body as the provider sent them, read back as the error the router sees.
#[tokio::test]
async fn refusals_match_the_shared_provider_responses() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../conformance/failure.v1.json");
    let data: Value = serde_json::from_slice(&std::fs::read(path).expect("read failure.v1.json")).expect("parse failure.v1.json");
    let cases = data["provider_responses"].as_array().expect("provider_responses");
    assert!(cases.len() >= 15, "only {} cases", cases.len());
    for c in cases {
        let name = c["name"].as_str().unwrap().to_string();
        let status = c["status"].as_u64().unwrap() as u16;
        let retry_after = c["retry_after"].as_str().map(str::to_string);
        let body = c["body"].as_str().unwrap().to_string();
        let base = upstream(move || {
            let mut r = Response::builder().status(status);
            if let Some(h) = &retry_after {
                r = r.header("retry-after", h);
            }
            r.body(Body::from(body.clone())).unwrap()
        })
        .await;
        let (events, error) = collect(&provider(&base, "k"), &invocation()).await;
        let error = error.unwrap_or_else(|| panic!("{name}: no error"));
        assert!(events.is_empty(), "{name}");
        let want = &c["expect"];
        assert_eq!(
            (error.definitive, error.retryable, error.code.as_str(), error.retry_after_ms),
            (
                want["definitive"].as_bool().unwrap(),
                want["retryable"].as_bool().unwrap(),
                want["code"].as_str().unwrap(),
                want["retry_after_ms"].as_u64()
            ),
            "{name}"
        );
    }
}
