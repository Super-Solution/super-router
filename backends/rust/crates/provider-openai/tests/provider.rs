//! The OpenAI adapter against fake upstreams: request shape, usage normalisation, failure classification, the
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
use super_router_provider_openai::OpenAi;

fn invocation() -> Invocation {
    serde_json::from_value(json!({
        "service": "s", "customer_id": "c", "payer_mode": "developer",
        "input": {"messages": [{"role": "user", "content": "hi"}]},
        "limits": {"max_output_tokens": 16, "timeout_ms": 5000},
    }))
    .unwrap()
}

fn offering() -> Offering {
    serde_json::from_value(json!({
        "id": "a", "provider": "openai", "model": "m", "upstream_model": "gpt-test", "region": "test", "labels": [],
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
    serve(Router::new().route("/v1/responses", post(move || async move { respond() }))).await
}

async fn stream_upstream(frames: &[String]) -> String {
    let body = frames.concat();
    upstream(move || sse(body.clone())).await
}

/// Everything the provider produced: the events before the first error, and that error.
async fn collect(provider: &OpenAi, input: &Invocation) -> (Vec<ProviderEvent>, Option<ProviderError>) {
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

fn provider(base: &str, key: &str) -> OpenAi {
    OpenAi::builder(key).base_url(base).build().unwrap()
}

fn shape(e: &ProviderError) -> (bool, bool, &str) {
    (e.definitive, e.retryable, e.code.as_str())
}

const GOOD_COMPLETED: &str = r#"{"type":"response.completed","response":{"id":"r","usage":{"input_tokens":1,"output_tokens":1}}}"#;

#[tokio::test]
async fn streams_text_and_normalises_usage() {
    let seen: Arc<Mutex<Option<(HeaderMap, Value)>>> = Arc::default();
    let sink = seen.clone();
    let frames = [
        frame(r#"{"type":"response.output_text.delta","delta":"Hel"}"#),
        frame(r#"{"type":"response.output_text.delta","delta":"lo"}"#),
        frame(r#"{"type":"response.completed","response":{"id":"resp_1","usage":{"input_tokens":100,"output_tokens":7,"input_tokens_details":{"cached_tokens":30,"cache_write_tokens":10}}}}"#),
    ]
    .concat();
    let app = Router::new().route(
        "/v1/responses",
        post(move |headers: HeaderMap, body: Bytes| async move {
            *sink.lock().unwrap() = Some((headers, serde_json::from_slice(&body).unwrap()));
            sse(frames)
        }),
    );
    let base = serve(app).await;
    let (events, error) = collect(&provider(&base, "sk-test"), &invocation()).await;
    assert_eq!(error, None);
    assert_eq!(
        events,
        vec![
            ProviderEvent::Delta { text: "Hel".into() },
            ProviderEvent::Delta { text: "lo".into() },
            ProviderEvent::Usage {
                usage: Meters { input: 60, cached: 30, cache_write: 10, output: 7, ..Meters::default() },
                upstream_id: "resp_1".into()
            },
            ProviderEvent::Done { completion: Some("succeeded".into()) },
        ]
    );
    let (headers, body) = seen.lock().unwrap().take().expect("the upstream was called");
    assert_eq!(headers["authorization"], "Bearer sk-test");
    assert_eq!(headers["content-type"], "application/json");
    assert_eq!(
        body,
        json!({"model": "gpt-test", "input": [{"role": "user", "content": "hi"}], "max_output_tokens": 16, "store": false, "stream": true})
    );
}

#[tokio::test]
async fn an_incomplete_response_is_partial() {
    let base =
        stream_upstream(&[frame(r#"{"type":"response.incomplete","response":{"id":"r","usage":{"input_tokens":1,"output_tokens":2}}}"#)])
            .await;
    let (events, error) = collect(&provider(&base, "k"), &invocation()).await;
    assert_eq!(error, None);
    assert_eq!(events.last(), Some(&ProviderEvent::Done { completion: Some("partial".into()) }));
}

#[tokio::test]
async fn other_events_and_unknown_fields_are_ignored() {
    let base = stream_upstream(&[
        frame(r#"{"type":"response.created","response":{"id":"r"},"extra":[1,2]}"#),
        frame(r#"{"type":"response.output_text.delta","delta":"a","logprobs":null}"#),
        frame(r#"{"type":null}"#),
        frame(GOOD_COMPLETED),
    ])
    .await;
    let (events, error) = collect(&provider(&base, "k"), &invocation()).await;
    assert_eq!(error, None);
    assert_eq!(events.len(), 3);
}

#[tokio::test]
async fn nothing_is_read_after_the_terminal_event() {
    let base = stream_upstream(&[frame(GOOD_COMPLETED), frame("not json at all")]).await;
    let (_, error) = collect(&provider(&base, "k"), &invocation()).await;
    assert_eq!(error, None);
}

fn status(code: u16) -> impl Fn() -> Response + Clone + Send + Sync + 'static {
    move || Response::builder().status(StatusCode::from_u16(code).unwrap()).body(Body::empty()).unwrap()
}

fn canned(body: String) -> impl Fn() -> Response + Clone + Send + Sync + 'static {
    move || sse(body.clone())
}

/// A named failure: how the fake upstream answers, the key, the invocation and the expected (definitive, retryable, code).
type Case<'a> = (&'a str, Box<dyn Fn() -> Response + Send + Sync>, &'a str, Invocation, (bool, bool, &'a str));

#[tokio::test]
async fn failures_are_classified() {
    let mut task = invocation();
    task.input.task = Some(serde_json::from_value(json!({"kind": "image", "prompt": "x"})).unwrap());
    let ok = invocation();
    let cases: Vec<Case> = vec![
        ("rate limited", Box::new(status(429)), "k", ok.clone(), (true, true, "upstream_http_429")),
        ("bad request", Box::new(status(400)), "k", ok.clone(), (true, false, "upstream_http_400")),
        ("unauthorized", Box::new(status(401)), "k", ok.clone(), (true, false, "upstream_http_401")),
        ("forbidden", Box::new(status(403)), "k", ok.clone(), (true, false, "upstream_http_403")),
        ("not found", Box::new(status(404)), "k", ok.clone(), (true, false, "upstream_http_404")),
        ("unprocessable", Box::new(status(422)), "k", ok.clone(), (true, false, "upstream_http_422")),
        ("server error is unknown", Box::new(status(500)), "k", ok.clone(), (false, false, "upstream_http_500")),
        ("bad gateway is unknown", Box::new(status(502)), "k", ok.clone(), (false, false, "upstream_http_502")),
        ("timeout status is unknown", Box::new(status(408)), "k", ok.clone(), (false, false, "upstream_http_408")),
        ("conflict is unknown", Box::new(status(409)), "k", ok.clone(), (false, false, "upstream_http_409")),
        ("no credentials", Box::new(status(200)), "", ok.clone(), (true, false, "credentials_missing")),
        ("a key that cannot be a header", Box::new(status(200)), "bad\nkey", ok.clone(), (true, false, "request_invalid")),
        ("tasks unsupported", Box::new(status(200)), "k", task, (true, false, "unsupported_task")),
        (
            "truncated stream",
            Box::new(canned(frame(r#"{"type":"response.output_text.delta","delta":"a"}"#))),
            "k",
            ok.clone(),
            (false, false, "stream_interrupted"),
        ),
        (
            "unterminated frame is dropped",
            Box::new(canned(r#"data: {"type":"response.completed"}"#.into())),
            "k",
            ok.clone(),
            (false, false, "stream_interrupted"),
        ),
        ("empty 200", Box::new(status(200)), "k", ok.clone(), (false, false, "stream_interrupted")),
        ("bad event json", Box::new(canned(frame("not json"))), "k", ok.clone(), (false, false, "invalid_upstream_event")),
        ("event that is not an object", Box::new(canned(frame("[]"))), "k", ok.clone(), (false, false, "invalid_upstream_event")),
        (
            "delta of the wrong type",
            Box::new(canned(frame(r#"{"type":"response.output_text.delta","delta":5}"#))),
            "k",
            ok.clone(),
            (false, false, "invalid_upstream_event"),
        ),
        (
            "completed without usage",
            Box::new(canned(frame(r#"{"type":"response.completed","response":{"id":"r"}}"#))),
            "k",
            ok.clone(),
            (false, false, "usage_unknown"),
        ),
        (
            "completed without a response",
            Box::new(canned(frame(r#"{"type":"response.completed"}"#))),
            "k",
            ok.clone(),
            (false, false, "usage_unknown"),
        ),
        (
            "usage without counters is not free",
            Box::new(canned(frame(r#"{"type":"response.completed","response":{"id":"r","usage":{}}}"#))),
            "k",
            ok.clone(),
            (false, false, "invalid_usage"),
        ),
        (
            "usage without output",
            Box::new(canned(frame(r#"{"type":"response.completed","response":{"id":"r","usage":{"input_tokens":3}}}"#))),
            "k",
            ok.clone(),
            (false, false, "invalid_usage"),
        ),
        (
            "usage without input",
            Box::new(canned(frame(r#"{"type":"response.completed","response":{"id":"r","usage":{"output_tokens":3}}}"#))),
            "k",
            ok.clone(),
            (false, false, "invalid_usage"),
        ),
        (
            "null counters",
            Box::new(canned(frame(
                r#"{"type":"response.completed","response":{"id":"r","usage":{"input_tokens":null,"output_tokens":null}}}"#,
            ))),
            "k",
            ok.clone(),
            (false, false, "invalid_usage"),
        ),
        (
            "fractional counter",
            Box::new(canned(frame(
                r#"{"type":"response.completed","response":{"id":"r","usage":{"input_tokens":3.5,"output_tokens":1}}}"#,
            ))),
            "k",
            ok.clone(),
            (false, false, "invalid_upstream_event"),
        ),
        (
            "string counter",
            Box::new(canned(frame(
                r#"{"type":"response.completed","response":{"id":"r","usage":{"input_tokens":"3","output_tokens":1}}}"#,
            ))),
            "k",
            ok.clone(),
            (false, false, "invalid_upstream_event"),
        ),
        (
            "negative output",
            Box::new(canned(frame(r#"{"type":"response.completed","response":{"id":"r","usage":{"input_tokens":3,"output_tokens":-1}}}"#))),
            "k",
            ok.clone(),
            (false, false, "invalid_usage"),
        ),
        (
            "negative input after cache",
            Box::new(canned(frame(
                r#"{"type":"response.completed","response":{"id":"r","usage":{"input_tokens":1,"output_tokens":1,"input_tokens_details":{"cached_tokens":5}}}}"#,
            ))),
            "k",
            ok.clone(),
            (false, false, "invalid_usage"),
        ),
        (
            "negative cache counter",
            Box::new(canned(frame(
                r#"{"type":"response.completed","response":{"id":"r","usage":{"input_tokens":9,"output_tokens":1,"input_tokens_details":{"cached_tokens":-2}}}}"#,
            ))),
            "k",
            ok.clone(),
            (false, false, "invalid_usage"),
        ),
        (
            "cache counters overflow",
            Box::new(canned(frame(
                r#"{"type":"response.completed","response":{"id":"r","usage":{"input_tokens":1,"output_tokens":1,"input_tokens_details":{"cached_tokens":-9223372036854775807,"cache_write_tokens":-9223372036854775807}}}}"#,
            ))),
            "k",
            ok.clone(),
            (false, false, "invalid_usage"),
        ),
        (
            "counter beyond a safe integer",
            Box::new(canned(frame(
                r#"{"type":"response.completed","response":{"id":"r","usage":{"input_tokens":9007199254740993,"output_tokens":1}}}"#,
            ))),
            "k",
            ok.clone(),
            (false, false, "invalid_usage"),
        ),
        ("upstream error event", Box::new(canned(frame(r#"{"type":"error"}"#))), "k", ok.clone(), (false, false, "upstream_incomplete")),
        (
            "failed response",
            Box::new(canned(frame(r#"{"type":"response.failed","response":{"id":"r"}}"#))),
            "k",
            ok.clone(),
            (false, false, "upstream_incomplete"),
        ),
    ];
    for (name, respond, key, input, want) in cases {
        let respond: Arc<dyn Fn() -> Response + Send + Sync> = Arc::from(respond);
        let base = serve(Router::new().route("/v1/responses", post(move || async move { respond() }))).await;
        let (_, error) = collect(&provider(&base, key), &input).await;
        let error = error.unwrap_or_else(|| panic!("{name}: no error"));
        assert_eq!(shape(&error), want, "{name}");
    }
}

#[tokio::test]
async fn a_failure_after_output_still_reports_the_output_first() {
    let base = stream_upstream(&[frame(r#"{"type":"response.output_text.delta","delta":"a"}"#), frame(r#"{"type":"error"}"#)]).await;
    let (events, error) = collect(&provider(&base, "k"), &invocation()).await;
    assert_eq!(events, vec![ProviderEvent::Delta { text: "a".into() }]);
    assert_eq!(shape(&error.unwrap()), (false, false, "upstream_incomplete"));
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
    let provider = OpenAi::builder("k")
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
        "/v1/responses",
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
        "/v1/responses",
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

#[tokio::test]
async fn dropping_the_stream_abandons_the_upstream_call() {
    let closed = Arc::new(AtomicBool::new(false));
    let base = hanging_upstream(closed.clone(), Some(frame(r#"{"type":"response.output_text.delta","delta":"a"}"#))).await;
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
        "/v1/responses",
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
    let base = hanging_upstream(closed.clone(), Some(frame(r#"{"type":"response.output_text.delta","delta":"a"}"#))).await;
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
    let body = frame(GOOD_COMPLETED);
    let app = Router::new().route(
        "/v1/responses",
        post(move |headers: HeaderMap| {
            let (sink, body) = (sink.clone(), body.clone());
            async move {
                sink.lock().unwrap().push(headers["authorization"].to_str().unwrap().to_string());
                sse(body)
            }
        }),
    );
    let base = serve(app).await;
    let current = Arc::new(Mutex::new("one".to_string()));
    let source = current.clone();
    let provider = OpenAi::builder_with_key_source(Arc::new(move || source.lock().unwrap().clone())).base_url(base).build().unwrap();
    assert_eq!(collect(&provider, &invocation()).await.1, None);
    *current.lock().unwrap() = "two".to_string();
    assert_eq!(collect(&provider, &invocation()).await.1, None);
    assert_eq!(*seen.lock().unwrap(), ["Bearer one", "Bearer two"]);
}

#[tokio::test]
async fn the_key_and_the_response_body_never_appear_in_errors_or_debug_output() {
    let secret = "sk-very-secret-value";
    let base = upstream(|| Response::builder().status(401).body(Body::from("echoed body: sk-very-secret-value")).unwrap()).await;
    let provider = provider(&base, secret);
    let (_, error) = collect(&provider, &invocation()).await;
    let error = error.unwrap();
    for text in [format!("{error:?}"), error.to_string(), format!("{provider:?}")] {
        assert!(!text.contains(secret) && !text.contains("echoed"), "{text}");
    }
}

#[tokio::test]
async fn a_trailing_slash_on_the_base_url_is_harmless() {
    let base = stream_upstream(&[frame(GOOD_COMPLETED)]).await;
    let provider = OpenAi::builder("k").base_url(format!("{base}/")).build().unwrap();
    assert_eq!(collect(&provider, &invocation()).await.1, None);
}
