//! Tool calling over the HTTP API, against the in-memory store and the scripted tool provider: the event order of
//! a streamed call, the next turn, idempotency, refusing a provider that cannot carry tools, a call cut off by the
//! output limit, an undeclared call, and the inference body limit. A port of the Go `server/tools_test.go`.

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Map, Value};
use super_router_contracts::{
    parse_service, Invocation, Meters, Offering, Principal, Provider, ProviderCapabilities, ProviderEvent, ProviderStream,
};
use super_router_router::{DataStore, MockProvider, MockToolProvider, Router};
use super_router_server::{handler, Key, Options, StaticKeys, MAX_BODY_BYTES, MAX_INFERENCE_BODY_BYTES};
use super_router_store_memory::MemoryStore;
use tower::ServiceExt;

const DIGEST_KEY: &str = "conformance-digest-key-0123456789abcdef";
const LOOKUP: &str =
    r#"{"name":"lookup","description":"Find a thing","input_schema":{"type":"object","properties":{"q":{"type":"string"}}}}"#;

fn owner() -> Principal {
    Principal {
        workspace: "ws".into(),
        environment: "test".into(),
        customer: "c1".into(),
        scopes: ["invoke", "service:write", "billing:write", "billing:read", "routing:write"].map(String::from).to_vec(),
        ..Principal::default()
    }
}

fn with_tools() -> String {
    format!(
        r#"{{"service":"chat","customer_id":"c1","input":{{"messages":[{{"role":"user","content":"Find it"}}],"tools":[{LOOKUP}]}},"limits":{{"max_output_tokens":32}}}}"#
    )
}

fn plain() -> String {
    r#"{"service":"chat","customer_id":"c1","input":{"messages":[{"role":"user","content":"Hi"}]},"limits":{"max_output_tokens":32}}"#
        .to_string()
}

fn streaming(body: &str) -> String {
    format!("{},\"stream\":true}}", body.strip_suffix('}').unwrap())
}

fn offering(id: &str, provider: &str) -> String {
    format!(
        r#"{{"id":"{id}","provider":"{provider}","model":"mock-model","upstream_model":"mock-model","region":"test","labels":[],"enabled":true,"source":"byok","max_input_tokens":1024,"max_output_tokens":128,"price":{{"asset":"USD","input":"1","cached":"1","cache_write":"1","output":"1","per":1}}}}"#
    )
}

struct Fixture {
    app: axum::Router,
    store: Arc<MemoryStore>,
}

async fn setup(providers: Vec<(&str, Arc<dyn Provider>)>, offerings: &[String]) -> Fixture {
    let default = [offering("a", "mock")];
    let offerings = if offerings.is_empty() { &default[..] } else { offerings };
    let service = parse_service(
        format!(
            r#"{{"id":"chat","version":1,"mode":"pool","regions":["test"],"price":{{"asset":"credit:demo","input":"2","cached":"1","cache_write":"2","output":"3"}},"max_attempts":2,"max_provider_cost":"10000","max_output_tokens":128,"weights":{{"cost":1,"latency":0,"reliability":0,"quality":0}},"cost_scale":"10000","latency_scale_ms":1000,"offerings":[{}]}}"#,
            offerings.join(",")
        )
        .as_bytes(),
    )
    .unwrap();
    let store = Arc::new(MemoryStore::default());
    store.publish(&owner(), &service, "", None).await.unwrap();
    store.grant(&owner(), "customer_sale", "credit:demo", "100000", "g1", None).await.unwrap();
    let providers: HashMap<String, Arc<dyn Provider>> = providers.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
    let router = Arc::new(Router::new(store.clone(), providers, DIGEST_KEY).unwrap());
    let auth = StaticKeys::new([Key { secret: "owner-key".into(), principal: owner() }]);
    Fixture { app: handler(Options::new(router, store.clone(), Arc::new(auth))), store }
}

struct Reply {
    status: StatusCode,
    body: Vec<u8>,
}

impl Reply {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|e| panic!("not JSON ({e}): {}", String::from_utf8_lossy(&self.body)))
    }

    /// The `data:` payloads of a stream, in order.
    fn events(&self) -> Vec<Value> {
        String::from_utf8_lossy(&self.body)
            .lines()
            .filter_map(|l| l.strip_prefix("data: "))
            .map(|d| serde_json::from_str(d).unwrap())
            .collect()
    }
}

fn types(events: &[Value]) -> Vec<&str> {
    events.iter().map(|e| e["type"].as_str().unwrap()).collect()
}

impl Fixture {
    async fn call(&self, path: &str, idem: &str, payload: String) -> Reply {
        let idem = if idem.is_empty() { uuid() } else { idem.to_string() };
        let req = Request::builder()
            .method(Method::POST)
            .uri(path)
            .header(header::AUTHORIZATION, "Bearer owner-key")
            .header("idempotency-key", idem)
            .body(Body::from(payload))
            .unwrap();
        let res = self.app.clone().oneshot(req).await.unwrap();
        let status = res.status();
        Reply { status, body: res.into_body().collect().await.unwrap().to_bytes().to_vec() }
    }

    async fn post(&self, payload: String, idem: &str) -> Reply {
        self.call("/v1/inferences", idem, payload).await
    }

    async fn ok(&self, payload: String) -> Value {
        let reply = self.post(payload, "").await;
        assert_eq!(reply.status, StatusCode::OK, "{}", String::from_utf8_lossy(&reply.body));
        reply.json()
    }

    /// Held and balance of every account, as `held/balance`.
    async fn held(&self) -> Vec<String> {
        self.store.balance(&owner()).await.unwrap().into_iter().map(|r| format!("{}/{}", r.held, r.balance)).collect()
    }
}

/// A fresh idempotency key.
fn uuid() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!("key-{}", NEXT.fetch_add(1, Ordering::SeqCst))
}

fn args(q: &str) -> Map<String, Value> {
    json!({ "q": q }).as_object().unwrap().clone()
}

#[tokio::test]
async fn a_tool_call_streams_in_order_and_settles_once() {
    let f = setup(vec![("mock", Arc::new(MockToolProvider::new("success").with_args(args("a"))))], &[]).await;
    let list = f.post(streaming(&with_tools()), "").await.events();
    assert_eq!(
        types(&list),
        ["request.started", "output.tool_call.delta", "output.tool_call.delta", "output.tool_call.completed", "request.completed"]
    );
    let (first, second, done, end) = (&list[1], &list[2], &list[3], &list[4]);
    assert_eq!((first["name"].as_str(), first["index"].as_u64()), (Some("lookup"), Some(0)));
    assert!(second.get("name").is_none() && second.get("id").is_none(), "{second}");
    let joined = format!("{}{}", first["arguments_delta"].as_str().unwrap(), second["arguments_delta"].as_str().unwrap());
    assert_eq!(joined, r#"{"q":"a"}"#);
    assert_eq!((done["arguments"].as_str(), &done["id"]), (Some(r#"{"q":"a"}"#), &first["id"]));
    assert_eq!(end["finish_reason"], "tool_calls");
    assert_eq!(end["billing_status"], "settled");
    assert_eq!(end["usage"], json!({ "status": "confirmed", "meters": { "input": 10, "cached": 0, "cache_write": 0, "output": 5 } }));
    for row in f.held().await {
        assert!(row.starts_with("0/"), "hold left: {row}");
    }
}

#[tokio::test]
async fn a_call_signature_travels_with_the_call_and_comes_back_on_the_next_turn() {
    let signature = "c2lnLUE9";
    let f = setup(vec![("mock", Arc::new(MockToolProvider::new("success").with_args(args("a")).with_signature(signature)))], &[]).await;
    let list = f.post(streaming(&with_tools()), "").await.events();
    assert_eq!((list[1]["signature"].as_str(), list[3]["signature"].as_str()), (Some(signature), Some(signature)), "{list:?}");
    assert!(list[2].get("signature").is_none(), "only the first event of a call carries it: {}", list[2]);

    let first = f.ok(with_tools()).await;
    let call = &first["output"]["tool_calls"][0];
    assert_eq!(call["signature"], signature);
    // The next turn replays the call with its signature and is accepted as it stands.
    let replay = json!({
        "service": "chat", "customer_id": "c1",
        "input": { "messages": [
            { "role": "user", "content": "Find it" },
            { "role": "assistant", "tool_calls": [call] },
            { "role": "tool", "tool_call_id": call["id"], "content": "found" },
        ], "tools": [serde_json::from_str::<Value>(LOOKUP).unwrap()] },
        "limits": { "max_output_tokens": 32 },
    });
    let done = f.ok(replay.to_string()).await;
    assert_eq!(done["finish_reason"], "stop");
}

#[tokio::test]
async fn a_tool_call_answers_without_streaming_and_the_next_turn_finishes() {
    let f = setup(vec![("mock", Arc::new(MockToolProvider::new("success").with_args(args("a"))))], &[]).await;
    let first = f.ok(with_tools()).await;
    let call = &first["output"]["tool_calls"][0];
    assert_eq!(first["finish_reason"], "tool_calls");
    assert_eq!((call["name"].as_str(), call["arguments"].as_str()), (Some("lookup"), Some(r#"{"q":"a"}"#)));
    let id = call["id"].as_str().unwrap();
    let second = f
        .ok(format!(
            r#"{{"service":"chat","customer_id":"c1","input":{{"tools":[{LOOKUP}],"messages":[{{"role":"user","content":"Find it"}},{{"role":"assistant","tool_calls":[{call}]}},{{"role":"tool","tool_call_id":"{id}","content":"found"}}]}},"limits":{{"max_output_tokens":32}}}}"#
        ))
        .await;
    assert_eq!(second["finish_reason"], "stop");
    assert_eq!(second["output"]["content"], "The tool finished.");
    assert!(second["output"].get("tool_calls").is_none(), "{second}");
    assert_eq!(f.ok(plain()).await["finish_reason"], "stop");
}

#[tokio::test]
async fn the_tool_list_is_part_of_the_idempotency_digest() {
    let provider = Arc::new(MockToolProvider::new("success"));
    let f = setup(vec![("mock", provider.clone())], &[]).await;
    assert_eq!(f.post(with_tools(), "k").await.status, StatusCode::OK);
    let after = f.held().await;
    assert_eq!(f.post(with_tools(), "k").await.status, StatusCode::SEE_OTHER);
    let changed = with_tools().replacen(r#""name":"lookup""#, r#""name":"other""#, 1);
    assert_eq!(f.post(changed, "k").await.status, StatusCode::CONFLICT);
    assert_eq!(provider.calls(), 1);
    assert_eq!(f.held().await, after);
}

#[tokio::test]
async fn a_provider_without_tools_is_refused_before_anything_is_reserved() {
    let provider = Arc::new(MockProvider::new(""));
    let f = setup(vec![("mock", provider.clone())], &[]).await;
    let before = f.held().await;
    let image = r#"{"service":"chat","customer_id":"c1","input":{"messages":[{"role":"user","content":[{"type":"image","url":"https://e.example/a.png"}]}]},"limits":{"max_output_tokens":32}}"#;
    let history = r#"{"service":"chat","customer_id":"c1","input":{"messages":[{"role":"user","content":"Hi"},{"role":"assistant","tool_calls":[{"id":"a","name":"x","arguments":"{}"}]},{"role":"tool","tool_call_id":"a","content":"r"}]},"limits":{"max_output_tokens":32}}"#;
    for body in [with_tools(), image.to_string(), history.to_string()] {
        let reply = f.post(body, "").await;
        assert_eq!(
            (reply.status, reply.json()["error"]["code"].as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, Some("unsupported_capability"))
        );
    }
    assert_eq!(provider.calls(), 0);
    assert_eq!(f.held().await, before);
}

#[tokio::test]
async fn routing_skips_a_provider_that_cannot_carry_tools() {
    let (text, tools) = (Arc::new(MockProvider::new("")), Arc::new(MockToolProvider::new("success")));
    let f =
        setup(vec![("text", text.clone()), ("tools", tools.clone())], &[offering("a-text", "text"), offering("b-tools", "tools")]).await;
    f.ok(with_tools()).await;
    assert_eq!((text.calls(), tools.calls()), (0, 1));
    f.ok(plain()).await;
    assert_eq!(text.calls(), 1, "plain text did not reach the first offering");
}

#[tokio::test]
async fn a_truncated_call_is_never_complete() {
    let f = setup(vec![("mock", Arc::new(MockToolProvider::new("truncated").with_args(args("abcdef"))))], &[]).await;
    let list = f.post(streaming(&with_tools()), "").await.events();
    assert!(!types(&list).contains(&"output.tool_call.completed"), "a cut-off call was completed");
    let end = list.last().unwrap();
    assert_eq!((end["type"].as_str(), end["execution_status"].as_str()), (Some("request.failed"), Some("partial")));
    assert_eq!((end["finish_reason"].as_str(), end["usage"]["status"].as_str()), (Some("length"), Some("confirmed")));
    let plain = f.post(with_tools(), "").await.json();
    assert!(plain["output"].get("tool_calls").is_none(), "{plain}");
    assert_eq!(plain["finish_reason"], "length");
}

/// Calls a tool the request never declared.
struct Rogue;

impl Provider for Rogue {
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities { tools: true, images: false }
    }

    fn invoke(&self, _input: &Invocation, _offering: &Offering) -> ProviderStream {
        Box::pin(futures_util::stream::iter([
            Ok(ProviderEvent::ToolCall {
                index: 0,
                id: Some("x".into()),
                name: Some("rogue".into()),
                signature: None,
                arguments_delta: "{}".into(),
            }),
            Ok(ProviderEvent::Usage { usage: Meters { input: 1, output: 1, ..Meters::default() }, upstream_id: String::new() }),
            Ok(ProviderEvent::Done { completion: None }),
        ]))
    }
}

#[tokio::test]
async fn an_undeclared_tool_call_keeps_the_hold() {
    let f = setup(vec![("mock", Arc::new(Rogue))], &[]).await;
    let list = f.post(streaming(&plain()), "").await.events();
    let end = list.last().unwrap();
    assert_eq!(end["type"], "request.failed");
    assert_ne!(end["execution_status"], "succeeded");
    assert!(f.held().await.iter().any(|row| !row.starts_with("0/")), "the hold was released");
}

#[tokio::test]
async fn inference_bodies_have_their_own_limit() {
    let f = setup(vec![("mock", Arc::new(MockToolProvider::new("success")))], &[]).await;
    let large = "x".repeat(250_000);
    let accepted = f
        .post(
            format!(
                r#"{{"service":"chat","customer_id":"c1","input":{{"tools":[{LOOKUP}],"messages":[{{"role":"user","content":"Go"}},{{"role":"assistant","tool_calls":[{{"id":"a","name":"lookup","arguments":"{{}}"}},{{"id":"b","name":"lookup","arguments":"{{}}"}}]}},{{"role":"tool","tool_call_id":"a","content":"{large}"}},{{"role":"tool","tool_call_id":"b","content":"{large}"}}]}},"limits":{{"max_output_tokens":32}}}}"#
            ),
            "",
        )
        .await;
    assert_eq!(accepted.status, StatusCode::OK, "{}", String::from_utf8_lossy(&accepted.body));
    let refused = f.post(format!(r#"{{"pad":"{}"}}"#, "x".repeat(MAX_INFERENCE_BODY_BYTES)), "").await;
    assert_eq!(refused.status, StatusCode::PAYLOAD_TOO_LARGE);
    let grant = f.call("/v1/admin/credits/grants", "g", format!(r#"{{"pad":"{}"}}"#, "x".repeat(MAX_BODY_BYTES))).await;
    assert_eq!(grant.status, StatusCode::PAYLOAD_TOO_LARGE, "other routes keep 256 KB");
}
