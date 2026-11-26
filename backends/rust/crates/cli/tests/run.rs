//! The binary's `run`: configuration checks, a full request through real HTTP on the in-memory store, a clean
//! shutdown, and database authentication against PostgreSQL.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

const API_KEY: &str = "sr_0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn digest_key() -> String {
    "d".repeat(32)
}

fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let map: HashMap<String, String> = pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect();
    move |name| map.get(name).cloned()
}

struct Running {
    base: String,
    stop: CancellationToken,
    done: tokio::task::JoinHandle<Result<(), String>>,
}

async fn start(pairs: Vec<(&'static str, String)>) -> Running {
    let stop = CancellationToken::new();
    let (tx, rx) = oneshot::channel::<SocketAddr>();
    let shutdown = stop.clone();
    let map: HashMap<String, String> = pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
    let cfg = super_router_cli::Config::from_env(&move |name| map.get(name).cloned()).expect("valid test configuration");
    let done = tokio::spawn(super_router_cli::run(cfg, shutdown, Some(tx)));
    let addr =
        tokio::time::timeout(Duration::from_secs(20), rx).await.expect("the server did not start").expect("the server failed to start");
    Running { base: format!("http://{addr}"), stop, done }
}

impl Running {
    async fn stop(self) {
        self.stop.cancel();
        let result = tokio::time::timeout(Duration::from_secs(10), self.done).await.expect("the server did not shut down");
        assert_eq!(result.unwrap(), Ok(()));
    }
}

fn service() -> Value {
    json!({
        "id": "chat", "version": 1, "mode": "tier", "tier": "chat/low", "regions": ["test"], "required_labels": [],
        "price": {"asset": "credit:demo", "input": "2", "cached": "1", "cache_write": "2", "output": "3", "per": 1},
        "max_attempts": 2, "max_provider_cost": "10000", "max_output_tokens": 128,
        "weights": {"cost": 1, "latency": 0, "reliability": 0, "quality": 0}, "cost_scale": "10000", "latency_scale_ms": 1000,
        "offerings": [{
            "id": "a", "provider": "mock", "model": "mock-model", "upstream_model": "mock-model", "region": "test", "labels": [],
            "enabled": true, "source": "byok", "max_input_tokens": 1024, "max_output_tokens": 128,
            "price": {"asset": "USD", "input": "1", "cached": "1", "cache_write": "1", "output": "1", "per": 1}
        }]
    })
}

#[test]
fn weak_configuration_is_refused() {
    let digest = digest_key();
    let cases: Vec<Vec<(&str, &str)>> = vec![
        vec![],
        vec![("SUPERROUTER_API_KEY", "short"), ("SUPERROUTER_DIGEST_KEY", &digest)],
        vec![("SUPERROUTER_API_KEY", API_KEY), ("SUPERROUTER_DIGEST_KEY", "short")],
        // The right shape but upper-case hex is not what the control plane issues.
        vec![
            ("SUPERROUTER_API_KEY", "sr_0123456789ABCDEF0123456789abcdef0123456789abcdef0123456789abcdef"),
            ("SUPERROUTER_DIGEST_KEY", &digest),
        ],
        vec![("SUPERROUTER_AUTH", "database"), ("SUPERROUTER_DIGEST_KEY", &digest)],
        vec![("SUPERROUTER_AUTH", "magic"), ("SUPERROUTER_API_KEY", API_KEY), ("SUPERROUTER_DIGEST_KEY", &digest)],
    ];
    for case in cases {
        let getenv = env(&case);
        let error = super_router_cli::Config::from_env(&getenv).expect_err(&format!("accepted {case:?}"));
        assert!(!error.contains(API_KEY) && !error.contains(&digest), "the error echoes a secret: {error}");
    }
    let ok = env(&[("SUPERROUTER_API_KEY", API_KEY), ("SUPERROUTER_DIGEST_KEY", &digest)]);
    let cfg = super_router_cli::Config::from_env(&ok).unwrap();
    assert_eq!(
        (cfg.addr.as_str(), cfg.workspace.as_str(), cfg.environment.as_str(), cfg.customer.as_str()),
        ("127.0.0.1:3410", "default", "prod", "default")
    );
}

#[tokio::test]
async fn serves_a_request_end_to_end_on_the_memory_store_and_shuts_down_cleanly() {
    let server = start(vec![
        ("SUPERROUTER_API_KEY", API_KEY.into()),
        ("SUPERROUTER_DIGEST_KEY", digest_key()),
        ("ADDR", "127.0.0.1:0".into()),
        ("SUPERROUTER_MOCK", "1".into()),
    ])
    .await;
    let http = reqwest::Client::new();
    let call = |method: reqwest::Method, path: &str, key: &str, body: Option<Value>| {
        let mut req = http.request(method, format!("{}{path}", server.base)).bearer_auth(API_KEY).header("Idempotency-Key", key);
        if let Some(body) = body {
            req = req.json(&body);
        }
        async move {
            let res = req.send().await.unwrap();
            let status = res.status().as_u16();
            (status, res.json::<Value>().await.unwrap_or(Value::Null))
        }
    };
    assert_eq!(http.get(format!("{}/healthz", server.base)).send().await.unwrap().status(), 200);
    assert_eq!(http.get(format!("{}/v1/services", server.base)).send().await.unwrap().status(), 401, "no credentials, no service");
    let (status, body) = call(reqwest::Method::GET, "/v1/requests/x", "k", None).await;
    assert_eq!((status, body["error"]["code"].as_str()), (404, Some("resource_not_found")));

    assert_eq!(call(reqwest::Method::POST, "/v1/admin/service-versions", "pub-1", Some(service())).await.0, 201);
    assert_eq!(
        call(reqwest::Method::POST, "/v1/admin/credits/grants", "grant-1", Some(json!({"asset": "credit:demo", "amount": "100000"})))
            .await
            .0,
        201
    );
    let invocation = json!({
        "service": "chat", "customer_id": "default", "input": {"messages": [{"role": "user", "content": "Hello"}]},
        "limits": {"max_output_tokens": 32}
    });
    let (status, first) = call(reqwest::Method::POST, "/v1/inferences", "infer-1", Some(invocation.clone())).await;
    assert_eq!(status, 200, "{first}");
    assert_eq!((first["execution_status"].as_str(), first["billing_status"].as_str()), (Some("succeeded"), Some("settled")));
    assert_eq!(first["output"]["content"], "Hello from SuperRouter.");
    // The same key again charges nothing more.
    let (_, again) = call(reqwest::Method::POST, "/v1/inferences", "infer-1", Some(invocation)).await;
    assert_eq!(again["id"], first["id"]);
    let (status, balance) = call(reqwest::Method::GET, "/v1/admin/balance", "b", None).await;
    assert_eq!((status, balance["data"][0]["balance"].as_str(), balance["data"][0]["held"].as_str()), (200, Some("99965"), Some("0")));
    server.stop().await;
}

#[tokio::test]
async fn database_auth_refuses_a_well_formed_key_that_is_not_in_the_database() {
    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgresql://superrouter:local-development-only@127.0.0.1:25432/superrouter".into());
    if sqlx::PgPool::connect(&url).await.is_err() {
        assert!(std::env::var("SR_REQUIRE_POSTGRES").is_err(), "postgres required but unreachable");
        eprintln!("skipping: PostgreSQL is unreachable");
        return;
    }
    let server = start(vec![
        ("SUPERROUTER_AUTH", "database".into()),
        ("DATABASE_URL", url),
        ("SUPERROUTER_DIGEST_KEY", digest_key()),
        ("ADDR", "127.0.0.1:0".into()),
    ])
    .await;
    let res = reqwest::Client::new().get(format!("{}/v1/services", server.base)).bearer_auth(API_KEY).send().await.unwrap();
    assert_eq!(res.status(), 401);
    server.stop().await;
}

#[tokio::test]
async fn accepted_connections_have_tcp_nodelay() {
    use axum::serve::Listener as _;
    let raw = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = raw.local_addr().unwrap();
    // Without the wrapper the accepted socket keeps the operating system's default (Nagle on).
    let _client = tokio::net::TcpStream::connect(addr).await.unwrap();
    let (plain, _) = raw.accept().await.unwrap();
    assert!(!plain.nodelay().unwrap(), "the default must be Nagle on, or this test proves nothing");
    let mut listener = super_router_cli::nodelay(raw);
    let _client = tokio::net::TcpStream::connect(addr).await.unwrap();
    let (io, _) = listener.accept().await;
    assert!(io.nodelay().unwrap(), "a streamed answer is many small writes; Nagle would delay all but the first by ~40 ms");
}

#[test]
fn provider_and_health_options_are_checked_and_never_echoed() {
    let digest = digest_key();
    let base = [("SUPERROUTER_API_KEY", API_KEY), ("SUPERROUTER_DIGEST_KEY", digest.as_str())];
    let with = |extra: &[(&'static str, &'static str)]| {
        let mut pairs: Vec<(&str, &str)> = base.to_vec();
        pairs.extend_from_slice(extra);
        let getenv = env(&pairs);
        super_router_cli::Config::from_env(&getenv)
    };
    for (name, value) in [
        ("OPENAI_BASE_URL", "ftp://host"),
        ("OPENAI_BASE_URL", "llm.internal"),
        ("OPENAI_BASE_URL", "https://user:secret-pass@llm.internal"),
        ("OPENAI_BASE_URL", "https://llm.internal/v1"),
        ("OPENAI_BASE_URL", "https://llm.internal/openai/v1/"),
        ("ANTHROPIC_BASE_URL", "https://llm.internal/?key=secret-pass"),
        ("GEMINI_BASE_URL", "https://generativelanguage.googleapis.com/v1beta"),
        ("GEMINI_BASE_URL", "https://"),
        ("SUPERROUTER_HEALTH", "yes"),
        ("SUPERROUTER_HEALTH_MAX_MS", "1000"),
    ] {
        let error = with(&[(name, value)]).expect_err(&format!("accepted {name}={value}"));
        assert!(!error.contains("secret-pass"), "{name}: the value must not be echoed");
    }
    for bad in ["0", "-1", "abc", "86400001", "1.5"] {
        assert!(with(&[("SUPERROUTER_HEALTH", "on"), ("SUPERROUTER_HEALTH_MAX_MS", bad)]).is_err(), "accepted a cooldown of {bad:?}");
    }
    let cfg = with(&[
        ("OPENAI_BASE_URL", "https://llm.internal/openai/"),
        ("ANTHROPIC_BASE_URL", "http://127.0.0.1:9000"),
        ("SUPERROUTER_HEALTH", "on"),
        ("SUPERROUTER_HEALTH_RATE_LIMITED_MS", "250"),
    ])
    .unwrap();
    assert_eq!(cfg.openai_base_url.as_deref(), Some("https://llm.internal/openai"));
    assert_eq!(cfg.anthropic_base_url.as_deref(), Some("http://127.0.0.1:9000"));
    assert_eq!(cfg.gemini_base_url, None);
    let policy = cfg.health.expect("health is on");
    assert_eq!(
        (policy.rate_limited_ms, policy.credentials_rejected_ms),
        (250, 300_000),
        "named fields change, the rest keep their defaults"
    );
    assert_eq!(with(&[("SUPERROUTER_HEALTH", "off")]).unwrap().health, None);
    assert_eq!(with(&[]).unwrap().health, None, "off by default");
}

/// A fake provider host: answers every POST with `status` (an empty body) or, for 200, a short Anthropic stream, and
/// counts the requests it got.
async fn fake_host(status: u16, calls: std::sync::Arc<std::sync::atomic::AtomicUsize>) -> String {
    use axum::{body::Body, http::StatusCode, response::Response, routing::post, Router};
    let app = Router::new().fallback(post(move || {
        let calls = calls.clone();
        async move {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if status != 200 {
                return Response::builder().status(StatusCode::from_u16(status).unwrap()).body(Body::empty()).unwrap();
            }
            let frames = [
                r#"{"type":"message_start","message":{"id":"m","usage":{"input_tokens":3,"output_tokens":1}}}"#,
                r#"{"type":"content_block_delta","delta":{"type":"text_delta","text":"from the second offering"}}"#,
                r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":4}}"#,
                r#"{"type":"message_stop"}"#,
            ];
            let body: String = frames.iter().map(|f| format!("event: x\ndata: {f}\n\n")).collect();
            Response::builder().header("content-type", "text/event-stream").body(Body::from(body)).unwrap()
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await });
    format!("http://{addr}")
}

/// The shipped server end to end with real providers: the first offering's key is rejected by its host (401), the
/// request falls through to the second offering, and with `SUPERROUTER_HEALTH=on` the next request does not touch the
/// rejected offering at all, while without it every request pays the failed attempt.
#[tokio::test]
async fn a_rejected_key_is_skipped_only_when_the_health_book_is_on() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    for health in [true, false] {
        let (rejected, serving) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let (openai_base, anthropic_base) = (fake_host(401, rejected.clone()).await, fake_host(200, serving.clone()).await);
        let mut pairs: Vec<(&'static str, String)> = vec![
            ("SUPERROUTER_API_KEY", API_KEY.into()),
            ("SUPERROUTER_DIGEST_KEY", digest_key()),
            ("ADDR", "127.0.0.1:0".into()),
            ("OPENAI_API_KEY", "sk-wrong".into()),
            ("OPENAI_BASE_URL", openai_base),
            ("ANTHROPIC_API_KEY", "sk-right".into()),
            ("ANTHROPIC_BASE_URL", anthropic_base),
        ];
        if health {
            pairs.push(("SUPERROUTER_HEALTH", "on".into()));
        }
        let server = start(pairs).await;
        let http = reqwest::Client::new();
        let call = |method: reqwest::Method, path: &str, key: &str, body: Option<Value>| {
            let mut req = http.request(method, format!("{}{path}", server.base)).bearer_auth(API_KEY).header("Idempotency-Key", key);
            if let Some(body) = body {
                req = req.json(&body);
            }
            async move {
                let res = req.send().await.unwrap();
                let status = res.status().as_u16();
                (status, res.json::<Value>().await.unwrap_or(Value::Null))
            }
        };
        let mut two = service();
        let mut second = two["offerings"][0].clone();
        second["id"] = "b".into();
        second["provider"] = "anthropic".into();
        two["offerings"][0]["provider"] = "openai".into();
        two["offerings"].as_array_mut().unwrap().push(second);
        assert_eq!(call(reqwest::Method::POST, "/v1/admin/service-versions", "pub", Some(two)).await.0, 201);
        let grant = json!({"asset": "credit:demo", "amount": "100000"});
        assert_eq!(call(reqwest::Method::POST, "/v1/admin/credits/grants", "grant", Some(grant)).await.0, 201);
        let invocation = json!({
            "service": "chat", "customer_id": "default", "input": {"messages": [{"role": "user", "content": "Hello"}]},
            "limits": {"max_output_tokens": 32}
        });
        for n in 1..=3 {
            let (status, body) = call(reqwest::Method::POST, "/v1/inferences", &format!("infer-{n}"), Some(invocation.clone())).await;
            assert_eq!(status, 200, "health={health} request {n}: {body}");
            assert_eq!(body["execution_status"], "succeeded", "health={health} request {n}");
            assert_eq!(body["output"]["content"], "from the second offering");
        }
        assert_eq!(serving.load(Ordering::SeqCst), 3, "the healthy offering served every request (health={health})");
        // On: one failed attempt, then the rejected offering is left alone (its cooldown is minutes). Off: three.
        assert_eq!(rejected.load(Ordering::SeqCst), if health { 1 } else { 3 }, "health={health}");
        server.stop().await;
    }
}
