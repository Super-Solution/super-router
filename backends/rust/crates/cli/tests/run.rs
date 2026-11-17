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
