//! The HTTP API against the in-memory store and the deterministic mock provider: a port of the Go server tests,
//! plus the behaviours that only matter over a wire (endless bodies, vanished clients, failures after a stream opened).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use axum::body::{Body, Bytes};
use axum::http::{header, Method, Request, StatusCode};
use futures_util::stream;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use super_router_contracts::{parse_service, Meters, Offering, Principal, Provider, Service};
use super_router_router::{
    BalanceRow, DataStore, Decision, Leg, MockProvider, RequestRecord, Reservation, Router, Store, StoreError, StoreResult,
};
use super_router_server::{handler, Key, Options, StaticKeys, WindowsSource, MAX_INFERENCE_BODY_BYTES};
use super_router_store_memory::MemoryStore;
use tower::ServiceExt;

const DIGEST_KEY: &str = "conformance-digest-key-0123456789abcdef";

fn owner() -> Principal {
    Principal {
        workspace: "ws".into(),
        environment: "test".into(),
        customer: "c1".into(),
        scopes: ["invoke", "service:write", "billing:write", "billing:read", "routing:write"].map(String::from).to_vec(),
        ..Principal::default()
    }
}

fn reader() -> Principal {
    Principal {
        workspace: "ws".into(),
        environment: "test".into(),
        customer: "c1".into(),
        scopes: vec!["billing:read".into()],
        ..Principal::default()
    }
}

fn dataplane() -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../conformance/dataplane.v1.json");
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn chat_service() -> Service {
    parse_service(&serde_json::to_vec(&dataplane()["services"]["chat"]).unwrap()).unwrap()
}

/// A store whose `finish` and `get_service` can be made to fail, to reach the error paths a healthy store never takes.
struct Flaky {
    inner: Arc<MemoryStore>,
    fail_finish: AtomicBool,
    fail_get_service: AtomicBool,
}

fn broken() -> StoreError {
    StoreError::Internal("connection reset by peer at 10.0.0.7:5432".into())
}

#[async_trait]
impl Store for Flaky {
    async fn replay(&self, p: &Principal, key: &str, digest: &str) -> StoreResult<Option<RequestRecord>> {
        self.inner.replay(p, key, digest).await
    }
    async fn get_service(&self, p: &Principal, id: &str, version: Option<i64>) -> StoreResult<Service> {
        if self.fail_get_service.load(Ordering::SeqCst) {
            return Err(broken());
        }
        self.inner.get_service(p, id, version).await
    }
    async fn accounts(&self, p: &Principal, service: &Service) -> StoreResult<Vec<Leg>> {
        self.inner.accounts(p, service).await
    }
    async fn reserve(
        &self,
        p: &Principal,
        key: &str,
        digest: &str,
        service: &Service,
        legs: &[Leg],
        deadline: i64,
        decision: &Decision,
    ) -> StoreResult<Reservation> {
        self.inner.reserve(p, key, digest, service, legs, deadline, decision).await
    }
    async fn get(&self, p: &Principal, id: &str) -> StoreResult<RequestRecord> {
        self.inner.get(p, id).await
    }
    async fn begin(&self, p: &Principal, id: &str, offering: &Offering, bound: &str) -> StoreResult<String> {
        self.inner.begin(p, id, offering, bound).await
    }
    async fn output(&self, p: &Principal, id: &str) -> StoreResult<()> {
        self.inner.output(p, id).await
    }
    async fn attempt_result(
        &self,
        p: &Principal,
        id: &str,
        attempt: &str,
        status: &str,
        usage: Option<&Meters>,
        cost: Option<&str>,
        upstream: &str,
    ) -> StoreResult<()> {
        self.inner.attempt_result(p, id, attempt, status, usage, cost, upstream).await
    }
    async fn finish(&self, p: &Principal, id: &str, execution: &str, usage: Option<&Meters>, pending: bool) -> StoreResult<()> {
        if self.fail_finish.load(Ordering::SeqCst) {
            return Err(broken());
        }
        self.inner.finish(p, id, execution, usage, pending).await
    }
    async fn cancel(&self, p: &Principal, id: &str) -> StoreResult<()> {
        self.inner.cancel(p, id).await
    }
}

#[async_trait]
impl DataStore for Flaky {
    async fn publish(&self, p: &Principal, service: &Service, mode: &str, expected: Option<i64>) -> StoreResult<Service> {
        self.inner.publish(p, service, mode, expected).await
    }
    async fn list(&self, p: &Principal) -> StoreResult<Vec<Service>> {
        self.inner.list(p).await
    }
    async fn balance(&self, p: &Principal) -> StoreResult<Vec<BalanceRow>> {
        self.inner.balance(p).await
    }
    async fn grant(&self, p: &Principal, kind: &str, asset: &str, amount: &str, source: &str, expires: Option<i64>) -> StoreResult<String> {
        self.inner.grant(p, kind, asset, amount, source, expires).await
    }
    async fn disable(&self, p: &Principal, offering: &str) -> StoreResult<()> {
        self.inner.disable(p, offering).await
    }
    async fn recover(&self) -> StoreResult<usize> {
        self.inner.recover().await
    }
}

struct Fixture {
    app: axum::Router,
    store: Arc<MemoryStore>,
    flaky: Arc<Flaky>,
    mock: Arc<MockProvider>,
}

struct Reply {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: Bytes,
}

impl Reply {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|e| panic!("not JSON ({e}): {:?}", String::from_utf8_lossy(&self.body)))
    }

    fn code(&self) -> String {
        self.json()["error"]["code"].as_str().unwrap_or("").to_string()
    }

    fn header(&self, name: &str) -> &str {
        self.headers.get(name).and_then(|v| v.to_str().ok()).unwrap_or("")
    }
}

async fn setup(behavior: &str, delay: Duration) -> Fixture {
    setup_with(behavior, delay, None).await
}

async fn setup_with(behavior: &str, delay: Duration, windows: Option<Arc<dyn WindowsSource>>) -> Fixture {
    let store = Arc::new(MemoryStore::default());
    store.publish(&owner(), &chat_service(), "", None).await.unwrap();
    store.grant(&owner(), "customer_sale", "credit:demo", "100000", "g1", None).await.unwrap();
    let flaky = Arc::new(Flaky { inner: store.clone(), fail_finish: AtomicBool::new(false), fail_get_service: AtomicBool::new(false) });
    let mock = Arc::new(MockProvider::new(behavior).with_delay(delay));
    let providers: HashMap<String, Arc<dyn Provider>> = HashMap::from([("ok".to_string(), mock.clone() as Arc<dyn Provider>)]);
    let router = Arc::new(Router::new(flaky.clone(), providers, DIGEST_KEY).unwrap());
    let auth =
        StaticKeys::new([Key { secret: "owner-key".into(), principal: owner() }, Key { secret: "reader-key".into(), principal: reader() }]);
    let mut options = Options::new(router, flaky.clone(), Arc::new(auth));
    options.windows = windows;
    Fixture { app: handler(options), store, flaky, mock }
}

fn request(method: &str, path: &str, key: &str, idem: &str, payload: &str) -> Request<Body> {
    let mut req = Request::builder().method(Method::from_bytes(method.as_bytes()).unwrap()).uri(path);
    if !key.is_empty() {
        req = req.header(header::AUTHORIZATION, format!("Bearer {key}"));
    }
    if !idem.is_empty() {
        req = req.header("idempotency-key", idem);
    }
    req.body(Body::from(payload.to_string())).unwrap()
}

async fn collect(res: axum::response::Response) -> Reply {
    let (parts, body) = res.into_parts();
    Reply { status: parts.status, headers: parts.headers, body: body.collect().await.unwrap().to_bytes() }
}

impl Fixture {
    async fn send(&self, req: Request<Body>) -> Reply {
        collect(self.app.clone().oneshot(req).await.unwrap()).await
    }

    async fn call(&self, method: &str, path: &str, key: &str, idem: &str, payload: &str) -> Reply {
        self.send(request(method, path, key, idem, payload)).await
    }
}

fn body(extra: &str) -> String {
    format!(
        r#"{{"service":"chat","customer_id":"c1","input":{{"messages":[{{"role":"user","content":"Hello"}}]}},"limits":{{"max_output_tokens":32}}{extra}}}"#
    )
}

async fn request_id(f: &Fixture) -> String {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let [id] = f.store.request_ids(&owner()).as_slice() {
            return id.clone();
        }
        assert!(Instant::now() < deadline, "no request was admitted");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn billing_of(f: &Fixture, id: &str) -> String {
    f.store.get(&owner(), id).await.unwrap().billing
}

async fn wait_for_billing(f: &Fixture, id: &str, want: &str) {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let now = billing_of(f, id).await;
        if now == want {
            return;
        }
        assert!(Instant::now() < deadline, "billing is {now:?}, wanted {want:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn inference_settles_and_a_replay_redirects() {
    let f = setup("success", Duration::ZERO).await;
    let res = f.call("POST", "/v1/inferences", "owner-key", "k1", &body("")).await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(!res.header("x-request-id").is_empty());
    assert!(res.header("content-type").starts_with("application/json"));
    assert!(res.body.ends_with(b"\n"), "JSON bodies end with a newline like the reference servers");
    let out = res.json();
    assert_eq!((out["billing_status"].as_str(), out["execution_status"].as_str()), (Some("settled"), Some("succeeded")));
    assert_eq!(res.header("x-request-id"), out["id"]);

    let replay = f.call("POST", "/v1/inferences", "owner-key", "k1", &body("")).await;
    assert_eq!(replay.status, StatusCode::SEE_OTHER);
    assert_eq!(replay.header("location"), format!("/v1/requests/{}", out["id"].as_str().unwrap()));
    assert!(replay.body.is_empty());
    assert_eq!(f.mock.calls(), 1);

    let status = f.call("GET", &format!("/v1/requests/{}", out["id"].as_str().unwrap()), "owner-key", "", "").await;
    assert_eq!(status.json()["billing_status"], "settled");
    assert_eq!(status.json()["service_version"], 1);

    let balance = f.call("GET", "/v1/admin/balance", "owner-key", "", "").await.json();
    let rows = balance["data"].as_array().unwrap();
    assert_eq!((rows.len(), rows[0]["balance"].as_str()), (1, Some("99965")));
}

#[tokio::test]
async fn errors_have_the_reference_codes_and_never_reach_the_provider() {
    let f = setup("success", Duration::ZERO).await;
    let payload = body("");
    let c2 = payload.replacen("\"c1\"", "\"c2\"", 1);
    let unknown_service = payload.replacen("\"chat\"", "\"nope\"", 1);
    let extra = body(r#","extra":1"#);
    #[allow(clippy::type_complexity)]
    let cases: Vec<(&str, &str, &str, &str, &str, &str, u16, &str)> = vec![
        ("no key", "POST", "/v1/inferences", "", "k", &payload, 401, "invalid_key"),
        ("wrong key", "POST", "/v1/inferences", "nope", "k", &payload, 401, "invalid_key"),
        ("no idempotency key", "POST", "/v1/inferences", "owner-key", "", &payload, 400, "missing_idempotency_key"),
        ("not json", "POST", "/v1/inferences", "owner-key", "k", "{", 400, "invalid_json"),
        ("empty body", "POST", "/v1/inferences", "owner-key", "k", "", 400, "invalid_json"),
        ("unknown field", "POST", "/v1/inferences", "owner-key", "k", &extra, 400, "invalid_request"),
        ("scope", "POST", "/v1/inferences", "reader-key", "k", &payload, 403, "scope_denied"),
        ("other customer", "POST", "/v1/inferences", "owner-key", "k", &c2, 403, "scope_denied"),
        ("unknown service", "POST", "/v1/inferences", "owner-key", "k", &unknown_service, 404, "resource_not_found"),
        ("unknown route", "GET", "/v1/nothing", "owner-key", "", "", 404, "resource_not_found"),
        ("wrong method", "DELETE", "/v1/services", "owner-key", "", "", 404, "resource_not_found"),
        ("trailing slash", "GET", "/v1/services/", "owner-key", "", "", 404, "resource_not_found"),
        ("too deep under requests", "GET", "/v1/requests/a/b/c", "owner-key", "", "", 404, "resource_not_found"),
        ("post to a request", "POST", "/v1/requests/abc", "owner-key", "k", "{}", 404, "resource_not_found"),
        ("get a cancel", "GET", "/v1/requests/abc/cancel", "owner-key", "", "", 404, "resource_not_found"),
        (
            "credit rates under another prefix",
            "POST",
            "/x/admin/services/chat/credit-rates",
            "owner-key",
            "k",
            "{}",
            404,
            "resource_not_found",
        ),
        ("reader cannot list", "GET", "/v1/services", "reader-key", "", "", 403, "scope_denied"),
        ("balance scope", "GET", "/v1/admin/balance", "reader-key", "", "", 200, ""),
        ("unknown request", "GET", "/v1/requests/nope", "owner-key", "", "", 404, "resource_not_found"),
        ("healthz needs GET", "POST", "/healthz", "", "k", "{}", 401, "invalid_key"),
    ];
    for (name, method, path, key, idem, input, status, code) in cases {
        let res = f.call(method, path, key, idem, input).await;
        assert_eq!(res.status.as_u16(), status, "{name}");
        if !code.is_empty() {
            assert_eq!(res.code(), code, "{name}");
            let error = &res.json()["error"];
            assert_eq!(error["retryable"], false, "{name}");
        }
    }
    assert_eq!(f.mock.calls(), 0, "rejected requests reached the provider");
}

#[tokio::test]
async fn authorization_is_parsed_like_the_reference() {
    let f = setup("success", Duration::ZERO).await;
    let with = |value: &str| {
        Request::builder().method("GET").uri("/v1/services").header(header::AUTHORIZATION, value.to_string()).body(Body::empty()).unwrap()
    };
    // "Bearer " is stripped once, and only in that exact spelling; everything else is the token itself.
    assert_eq!(f.send(with("Bearer owner-key")).await.status, StatusCode::OK);
    assert_eq!(f.send(with("owner-key")).await.status, StatusCode::OK, "a bare token authenticates, as in Go and TypeScript");
    for bad in ["bearer owner-key", "Bearer  owner-key", "Bearer", "Bearer ", "Basic owner-key", "", "Bearer Bearer owner-key"] {
        let res = f.send(with(bad)).await;
        assert_eq!((res.status, res.code().as_str()), (StatusCode::UNAUTHORIZED, "invalid_key"), "{bad:?}");
    }
    let none = Request::builder().uri("/v1/services").body(Body::empty()).unwrap();
    assert_eq!(f.send(none).await.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn healthz_needs_no_credentials() {
    let f = setup("success", Duration::ZERO).await;
    let res = f.call("GET", "/healthz", "", "", "").await;
    assert_eq!((res.status, res.json()), (StatusCode::OK, json!({"status": "ok"})));
}

#[tokio::test]
async fn a_body_over_the_limit_is_refused() {
    let f = setup("success", Duration::ZERO).await;
    let huge = format!(r#"{{"x":"{}"}}"#, "a".repeat(MAX_INFERENCE_BODY_BYTES));
    let res = f.call("POST", "/v1/inferences", "owner-key", "k", &huge).await;
    assert_eq!((res.status, res.code().as_str()), (StatusCode::PAYLOAD_TOO_LARGE, "body_too_large"));
    // The same through a declared Content-Length, which is refused without reading anything.
    let declared = Request::builder()
        .method("POST")
        .uri("/v1/inferences")
        .header(header::AUTHORIZATION, "Bearer owner-key")
        .header("idempotency-key", "k2")
        .header(header::CONTENT_LENGTH, (MAX_INFERENCE_BODY_BYTES + 1).to_string())
        .body(Body::from_stream(stream::pending::<Result<Bytes, std::io::Error>>()))
        .unwrap();
    let res = tokio::time::timeout(Duration::from_secs(2), f.send(declared)).await.expect("a declared oversize body must not be read");
    assert_eq!(res.status, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn an_endless_chunked_body_is_cut_off_at_the_limit() {
    let f = setup("success", Duration::ZERO).await;
    let pulled = Arc::new(AtomicUsize::new(0));
    let counter = pulled.clone();
    // No Content-Length: the server cannot know the size, so it must count what it reads. The stream is "endless"
    // for any sane limit but ends after about 49 MB, so a server that buffers fails this test instead of the host.
    let endless = stream::iter(
        std::iter::repeat_with(move || {
            counter.fetch_add(8192, Ordering::SeqCst);
            Ok::<_, std::io::Error>(Bytes::from(vec![b'a'; 8192]))
        })
        .take(6000),
    );
    let req = Request::builder()
        .method("POST")
        .uri("/v1/inferences")
        .header(header::AUTHORIZATION, "Bearer owner-key")
        .header("idempotency-key", "k")
        .body(Body::from_stream(endless))
        .unwrap();
    let res = tokio::time::timeout(Duration::from_secs(5), f.send(req)).await.expect("an endless body was buffered instead of refused");
    assert_eq!((res.status, res.code().as_str()), (StatusCode::PAYLOAD_TOO_LARGE, "body_too_large"));
    let read = pulled.load(Ordering::SeqCst);
    assert!((MAX_INFERENCE_BODY_BYTES..=MAX_INFERENCE_BODY_BYTES + 2 * 8192).contains(&read), "read {read} bytes before giving up");
}

fn events(text: &str) -> Vec<(String, Value)> {
    text.split("\n\n")
        .filter(|b| !b.is_empty())
        .map(|block| {
            let mut lines = block.lines();
            let kind = lines.next().and_then(|l| l.strip_prefix("event: ")).expect("event line").to_string();
            let data = lines.next().and_then(|l| l.strip_prefix("data: ")).expect("data line");
            assert!(lines.next().is_none(), "one data line per event");
            (kind, serde_json::from_str(data).unwrap())
        })
        .collect()
}

#[tokio::test]
async fn streaming_sends_events_and_early_failures_stay_json() {
    let f = setup("success", Duration::ZERO).await;
    let res = f.call("POST", "/v1/inferences", "owner-key", "s1", &body(r#","stream":true"#)).await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(res.header("content-type"), "text/event-stream");
    assert_eq!(res.header("cache-control"), "no-cache");
    assert_eq!(res.header("x-accel-buffering"), "no");
    let seen = events(std::str::from_utf8(&res.body).unwrap());
    let kinds: Vec<&str> = seen.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(kinds, ["request.started", "output.delta", "request.completed"]);
    let id = res.header("x-request-id");
    assert!(!id.is_empty());
    for (i, (kind, data)) in seen.iter().enumerate() {
        assert_eq!(
            (data["type"].as_str(), data["request_id"].as_str(), data["sequence"].as_u64()),
            (Some(kind.as_str()), Some(id), Some(i as u64 + 1))
        );
    }
    assert_eq!(seen[0].1["service_version"], 1);
    assert_eq!(seen[1].1["text"], "Hello from SuperRouter.");
    assert_eq!((seen[2].1["execution_status"].as_str(), seen[2].1["billing_status"].as_str()), (Some("succeeded"), Some("settled")));
    assert_eq!(f.store.get(&owner(), id).await.unwrap().billing, "settled");

    // A failure before the request exists is still a JSON error with a real status.
    let bad = f.call("POST", "/v1/inferences", "owner-key", "s2", &body(r#","stream":true"#).replacen("\"chat\"", "\"nope\"", 1)).await;
    assert_eq!(bad.status, StatusCode::NOT_FOUND);
    assert!(bad.header("content-type").starts_with("application/json"));

    // A replayed streaming request redirects instead of streaming.
    let again = f.call("POST", "/v1/inferences", "owner-key", "s1", &body(r#","stream":true"#)).await;
    assert_eq!(again.status, StatusCode::SEE_OTHER);
    assert_eq!(f.mock.calls(), 1);
}

#[tokio::test]
async fn a_failure_after_the_stream_opened_is_its_last_event() {
    let f = setup("success", Duration::ZERO).await;
    f.flaky.fail_finish.store(true, Ordering::SeqCst);
    let res = f.call("POST", "/v1/inferences", "owner-key", "s1", &body(r#","stream":true"#)).await;
    assert_eq!(res.status, StatusCode::OK, "the stream was already open");
    let seen = events(std::str::from_utf8(&res.body).unwrap());
    let (kind, data) = seen.last().unwrap();
    assert_eq!(kind, "request.failed");
    assert_eq!(data["type"], "request.failed");
    assert_eq!(data["error"], json!({"code": "execution_unknown"}));
    assert_eq!(data["request_id"], res.header("x-request-id"));
    assert!(!String::from_utf8_lossy(&res.body).contains("10.0.0.7"), "internal detail leaked into the stream");
}

#[tokio::test]
async fn failures_after_reservation_name_the_request_and_internal_ones_stay_internal() {
    let f = setup("success", Duration::ZERO).await;
    f.flaky.fail_finish.store(true, Ordering::SeqCst);
    let res = f.call("POST", "/v1/inferences", "owner-key", "k1", &body("")).await;
    assert_eq!((res.status, res.code().as_str()), (StatusCode::SERVICE_UNAVAILABLE, "execution_unknown"));
    let id = request_id(&f).await;
    assert_eq!(res.json()["error"]["request_id"], id.as_str());

    // Before a reservation exists an internal failure is a bare internal_error, with no storage detail.
    let g = setup("success", Duration::ZERO).await;
    g.flaky.fail_get_service.store(true, Ordering::SeqCst);
    let res = g.call("POST", "/v1/inferences", "owner-key", "k1", &body("")).await;
    assert_eq!(res.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(res.json(), json!({"error": {"code": "internal_error", "retryable": false}}));
    assert!(!String::from_utf8_lossy(&res.body).contains("10.0.0.7"));

    // Routes that call the store directly map an internal failure the same way.
    let res = g.call("POST", "/v1/admin/routing/dry-run", "owner-key", "r1", &body("")).await;
    assert_eq!(res.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(res.json(), json!({"error": {"code": "internal_error", "retryable": false}}));
}

#[tokio::test]
async fn cancel_aborts_the_provider() {
    let f = Arc::new(setup("success", Duration::from_secs(5)).await);
    let running = tokio::spawn({
        let f = f.clone();
        async move { f.call("POST", "/v1/inferences", "owner-key", "c1", &body("")).await }
    });
    let id = request_id(&f).await;
    let started = Instant::now();
    let cancel = f.call("POST", &format!("/v1/requests/{id}/cancel"), "owner-key", "x", "{}").await;
    assert_eq!(cancel.status, StatusCode::ACCEPTED);
    assert_eq!(cancel.json(), json!({"id": id, "cancel_requested": true}));
    let out = tokio::time::timeout(Duration::from_secs(3), running).await.expect("cancel did not stop the provider").unwrap();
    assert!(started.elapsed() < Duration::from_secs(3));
    // The provider reported no result, so the outcome is unknown and the funds stay held for reconciliation.
    assert_ne!(out.json()["billing_status"], "settled", "a cancelled request must not settle: {}", out.json());
    assert_eq!(billing_of(&f, &id).await, "pending");
}

#[tokio::test]
async fn a_dropped_handler_future_keeps_the_funds_held() {
    let f = Arc::new(setup("success", Duration::from_secs(5)).await);
    let task = tokio::spawn({
        let f = f.clone();
        async move { f.call("POST", "/v1/inferences", "owner-key", "d1", &body("")).await }
    });
    let id = request_id(&f).await;
    // Dropping the future is what hyper does when the connection closes.
    task.abort();
    let _ = task.await;
    wait_for_billing(&f, &id, "pending").await;
    let rows = f.store.balance(&owner()).await.unwrap();
    assert_eq!(rows[0].balance, "100000", "nothing was charged");
    assert_ne!(rows[0].held, "0", "the reservation stays held for reconciliation");
}

#[tokio::test]
async fn a_client_that_closes_the_socket_keeps_the_funds_held() {
    use tokio::io::AsyncWriteExt;
    let f = Arc::new(setup("success", Duration::from_secs(5)).await);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = f.app.clone();
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let payload = body("");
    let mut socket = tokio::net::TcpStream::connect(addr).await.unwrap();
    let head = format!(
        "POST /v1/inferences HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer owner-key\r\nIdempotency-Key: d2\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{payload}",
        payload.len()
    );
    socket.write_all(head.as_bytes()).await.unwrap();
    let id = request_id(&f).await;
    drop(socket);
    wait_for_billing(&f, &id, "pending").await;
    server.abort();
}

#[tokio::test]
async fn a_streaming_client_that_goes_away_keeps_the_funds_held() {
    let f = Arc::new(setup("success", Duration::from_millis(300)).await);
    let res = f.app.clone().oneshot(request("POST", "/v1/inferences", "owner-key", "d3", &body(r#","stream":true"#))).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let id = request_id(&f).await;
    // The headers are out; the provider is still thinking. Dropping the body is the client leaving.
    drop(res);
    wait_for_billing(&f, &id, "pending").await;
}

#[tokio::test]
async fn publish_and_dry_run() {
    let f = setup("success", Duration::ZERO).await;
    let res = f.call("POST", "/v1/admin/routing/dry-run", "owner-key", "r1", &body("")).await;
    assert_eq!(res.status, StatusCode::OK);
    let out = res.json();
    let candidates = out["candidates"].as_array().unwrap();
    assert_eq!((candidates.len(), candidates[0]["id"].as_str()), (1, Some("a")));
    assert_eq!(out["excluded"], json!([]));
    assert!(candidates[0]["cost"].is_string());
    assert!(candidates[0]["score"].is_number());
    assert_eq!(f.call("POST", "/v1/admin/routing/dry-run", "reader-key", "r2", &body("")).await.status, StatusCode::FORBIDDEN);

    let bad = f.call("POST", "/v1/admin/service-versions", "owner-key", "p1", r#"{"id":"x"}"#).await;
    assert_eq!((bad.status, bad.code().as_str()), (StatusCode::BAD_REQUEST, "invalid_request"));

    let mut next = serde_json::to_value(chat_service()).unwrap();
    next["version"] = json!(2);
    let published = f.call("POST", "/v1/admin/service-versions", "owner-key", "p2", &next.to_string()).await;
    assert_eq!(published.status, StatusCode::CREATED);
    assert_eq!(published.json()["version"], 2);
    let listed = f.call("GET", "/v1/services", "owner-key", "", "").await.json();
    assert_eq!(listed["next_cursor"], Value::Null);
    let entry = &listed["data"][0];
    assert_eq!((entry["id"].as_str(), entry["version"].as_i64(), entry["tier"].as_str()), (Some("chat"), Some(2), Some("chat/low")));
    assert_eq!(entry["max_output_tokens"], 128);
    assert_eq!(entry["price"]["asset"], "credit:demo");
}

#[tokio::test]
async fn grant_endpoint() {
    let f = setup("success", Duration::ZERO).await;
    let grant = r#"{"asset":"credit:demo","amount":"500"}"#;
    let created = f.call("POST", "/v1/admin/credits/grants", "owner-key", "g-http", grant).await;
    assert_eq!(created.status, StatusCode::CREATED);
    assert!(created.json()["account"].as_str().unwrap().starts_with("acct_"));
    // The same Idempotency-Key with the same body does not fund twice; a different body conflicts.
    assert_eq!(f.call("POST", "/v1/admin/credits/grants", "owner-key", "g-http", grant).await.status, StatusCode::CREATED);
    let conflict = f.call("POST", "/v1/admin/credits/grants", "owner-key", "g-http", r#"{"asset":"credit:demo","amount":"501"}"#).await;
    assert_eq!(conflict.status, StatusCode::CONFLICT);
    let rows = f.call("GET", "/v1/admin/balance", "owner-key", "", "").await.json();
    assert_eq!(rows["data"][0]["balance"], "100500");
    for bad in [
        r#"{}"#,
        r#"{"asset":"credit:demo","amount":"0"}"#,
        r#"{"asset":"credit:demo","amount":"5","extra":1}"#,
        r#"{"asset":"credit:demo","amount":"5","kind":"x"}"#,
        r#"{"asset":"credit:demo","amount":5}"#,
        r#"{"asset":"credit:demo","amount":"5","expires_at":"tomorrow"}"#,
        r#"[]"#,
        r#"null"#,
    ] {
        let res = f.call("POST", "/v1/admin/credits/grants", "owner-key", "g-bad", bad).await;
        assert_eq!(res.status, StatusCode::BAD_REQUEST, "{bad}");
    }
    assert_eq!(f.call("POST", "/v1/admin/credits/grants", "reader-key", "g-r", grant).await.status, StatusCode::FORBIDDEN);
    // Optional fields: an explicit kind, and an expiry with a UTC offset.
    let expiring = r#"{"kind":"customer_sale","asset":"credit:demo","amount":"7","expires_at":"2030-01-01T08:00:00+08:00"}"#;
    assert_eq!(f.call("POST", "/v1/admin/credits/grants", "owner-key", "g-exp", expiring).await.status, StatusCode::CREATED);
}

fn units(n: &str) -> String {
    format!(r#"{{"units":"{n}"}}"#)
}

#[tokio::test]
async fn credit_rates_endpoint() {
    let f = setup("success", Duration::ZERO).await;
    let config =
        format!(r#"{{"asset":"credit:demo","tokens":{{"input":{u},"cached":{u},"cache_write":{u},"output":{u}}}}}"#, u = units("1000"));
    let post = |idem: &'static str, body: String| {
        let f = &f;
        async move { f.call("POST", "/v1/admin/services/chat/credit-rates", "owner-key", idem, &body).await }
    };
    let res = post("cr1", format!(r#"{{"expected_version":1,"config":{config}}}"#)).await;
    assert_eq!(res.status, StatusCode::CREATED);
    assert_eq!(res.json()["version"], 2);
    let stale = post("cr2", format!(r#"{{"expected_version":1,"config":{config}}}"#)).await;
    assert_eq!((stale.status, stale.code().as_str()), (StatusCode::CONFLICT, "service_version_conflict"));
    let cases = [
        (r#"{"expected_version":"2"}"#.to_string(), "invalid_service_version"),
        (format!(r#"{{"expected_version":2.5,"config":{config}}}"#), "invalid_service_version"),
        (format!(r#"{{"expected_version":1e2,"config":{config}}}"#), "invalid_service_version"),
        (r#"{"x":1}"#.to_string(), "invalid_credit_rates"),
        (r#"[]"#.to_string(), "invalid_credit_rates"),
        (r#"null"#.to_string(), "invalid_credit_rates"),
        (r#"{"expected_version":2,"config":{"asset":"credit:demo"}}"#.to_string(), "invalid_credit_rates"),
        (r#"{"expected_version":2,"config":{"asset":"other","tokens":{}}}"#.to_string(), "invalid_credit_rates"),
        (r#"{"expected_version":2}"#.to_string(), "invalid_credit_rates"),
    ];
    for (input, code) in cases {
        let res = post("cr-bad", input.clone()).await;
        assert_eq!(res.code(), code, "{input} -> {}", res.status);
    }
    let reader = f
        .call(
            "POST",
            "/v1/admin/services/chat/credit-rates",
            "reader-key",
            "cr3",
            &format!(r#"{{"expected_version":2,"config":{config}}}"#),
        )
        .await;
    assert_eq!(reader.status, StatusCode::FORBIDDEN);
    // The id is percent-decoded like the path it came from.
    let encoded = f
        .call(
            "POST",
            "/v1/admin/services/ch%61t/credit-rates",
            "owner-key",
            "cr4",
            &format!(r#"{{"expected_version":2,"config":{config}}}"#),
        )
        .await;
    assert_eq!(encoded.status, StatusCode::CREATED);
    assert_eq!(encoded.json()["version"], 3);
}

#[tokio::test]
async fn static_keys_refuse_an_empty_token() {
    use super_router_router::Authenticator;
    let auth = StaticKeys::new([Key { secret: String::new(), principal: owner() }, Key { secret: "real".into(), principal: reader() }]);
    let err = auth.authenticate("").await.unwrap_err();
    assert_eq!((err.code.as_str(), err.status), ("invalid_key", 401));
    assert_eq!(auth.authenticate("real").await.unwrap().scopes, reader().scopes);
    assert!(auth.authenticate("rea").await.is_err() && auth.authenticate("real ").await.is_err());
    // And over HTTP, an unset secret never lets a credential-less request in.
    let f = setup("success", Duration::ZERO).await;
    let router = Arc::new(Router::new(f.store.clone(), HashMap::new(), DIGEST_KEY).unwrap());
    let blank = Options::new(router, f.store.clone(), Arc::new(StaticKeys::new([Key { secret: String::new(), principal: owner() }])));
    let res = collect(handler(blank).oneshot(Request::builder().uri("/v1/services").body(Body::empty()).unwrap()).await.unwrap()).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
}

struct FixedWindows;

#[async_trait]
impl WindowsSource for FixedWindows {
    async fn windows_json(&self, p: &Principal) -> StoreResult<Value> {
        Ok(json!([{ "customer": p.customer, "window": "session" }]))
    }
}

#[tokio::test]
async fn windows_need_a_scope_and_a_store_that_has_them() {
    let without = setup("success", Duration::ZERO).await;
    assert_eq!(without.call("GET", "/v1/windows", "owner-key", "", "").await.code(), "resource_not_found");
    let with = setup_with("success", Duration::ZERO, Some(Arc::new(FixedWindows))).await;
    let ok = with.call("GET", "/v1/windows", "reader-key", "", "").await;
    assert_eq!((ok.status, ok.json()), (StatusCode::OK, json!({"windows": [{"customer": "c1", "window": "session"}]})));
    let denied = Principal { scopes: vec!["invoke".into()], ..reader() };
    let store = with.store.clone();
    let router = Arc::new(Router::new(store.clone(), HashMap::new(), DIGEST_KEY).unwrap());
    let mut options = Options::new(router, store, Arc::new(StaticKeys::new([Key { secret: "narrow".into(), principal: denied }])));
    options.windows = Some(Arc::new(FixedWindows));
    let res = collect(handler(options).oneshot(request("GET", "/v1/windows", "narrow", "", "")).await.unwrap()).await;
    assert_eq!((res.status, res.code().as_str()), (StatusCode::FORBIDDEN, "scope_denied"));
}

#[tokio::test]
async fn it_mounts_under_a_prefix() {
    let f = setup("success", Duration::ZERO).await;
    let app = axum::Router::new().nest("/router", f.app.clone());
    let res = collect(app.oneshot(request("GET", "/router/v1/services", "owner-key", "", "")).await.unwrap()).await;
    assert_eq!(res.status, StatusCode::OK);
}
