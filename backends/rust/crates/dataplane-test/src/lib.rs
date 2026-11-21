//! Runs `conformance/dataplane.v1.json` against any [`DataStore`]. The same file drives the TypeScript, Go and
//! Python implementations, so every store behaves identically. [`run_tool_cases`] does the same for the provider
//! sections of `conformance/tools.v1.json`.

mod providers;

pub use providers::run_tool_cases;

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::{json, Map, Value};
use super_router_contracts::{parse_service, Meters, Principal, Provider, Service};
use super_router_router::{Clock, DataStore, Decision, Leg, MockProvider, Router};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Keys the idempotency digest in every scenario.
pub const DIGEST_KEY: &str = "conformance-digest-key-0123456789abcdef";

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// One funding bucket as the scenarios compare it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BucketRow {
    pub source: String,
    pub remaining: String,
    pub held: String,
}

/// A store under test plus the means to control and observe it.
pub struct Harness {
    pub store: Arc<dyn DataStore>,
    /// "Now" for the store: virtual for in-memory stores, the real one for SQL stores.
    pub clock: Clock,
    /// Moves time forward by this many milliseconds (virtually or by sleeping).
    pub advance: Box<dyn Fn(i64) -> BoxFuture<()> + Send + Sync>,
    /// Lists a tenant's funding buckets ordered by source.
    pub buckets: Box<dyn Fn(Principal) -> BoxFuture<Vec<BucketRow>> + Send + Sync>,
}

pub fn load() -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../conformance/dataplane.v1.json");
    serde_json::from_slice(&std::fs::read(path).expect("read dataplane.v1.json")).expect("parse dataplane.v1.json")
}

/// Deep-merges `patch` over `base`, objects recursively, anything else replaced.
fn merge(base: &Value, patch: &Value) -> Value {
    match (base, patch) {
        (Value::Object(b), Value::Object(p)) => {
            let mut out = b.clone();
            for (k, v) in p {
                let merged = out.get(k).map_or_else(|| v.clone(), |existing| merge(existing, v));
                out.insert(k.clone(), merged);
            }
            Value::Object(out)
        }
        // A step without a patch changes nothing.
        (b, Value::Null) => b.clone(),
        (_, p) => p.clone(),
    }
}

fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

/// Partial match: strings may be `$name` references to earlier ids or `>0`; objects match by the keys they name.
fn matches(actual: &Value, expected: &Value, refs: &HashMap<String, String>, path: &str, out: &mut Vec<String>) {
    match expected {
        Value::String(e) => {
            if let Some(name) = e.strip_prefix('$') {
                let want = refs.get(name).map_or("", String::as_str);
                if actual.as_str() != Some(want) {
                    out.push(format!("{path}: {actual}, want reference {e} = {want}"));
                }
            } else if e == ">0" {
                let positive = actual.as_str().and_then(|s| s.parse::<u128>().ok()).is_some_and(|n| n > 0);
                if !positive {
                    out.push(format!("{path}: {actual} is not > 0"));
                }
            } else if actual.as_str() != Some(e) {
                out.push(format!("{path}: {actual}, want {e:?}"));
            }
        }
        Value::Array(e) => match actual.as_array() {
            Some(a) if a.len() == e.len() => {
                for (i, (a, e)) in a.iter().zip(e).enumerate() {
                    matches(a, e, refs, &format!("{path}[{i}]"), out);
                }
            }
            _ => out.push(format!("{path}: {actual}, want {} elements {expected}", e.len())),
        },
        Value::Object(e) => match actual.as_object() {
            Some(a) => {
                for (k, v) in e {
                    matches(a.get(k).unwrap_or(&Value::Null), v, refs, &format!("{path}.{k}"), out);
                }
            }
            None => out.push(format!("{path}: {actual}, want object")),
        },
        Value::Number(e) => {
            if actual.as_f64() != e.as_f64() {
                out.push(format!("{path}: {actual}, want {e}"));
            }
        }
        other => {
            if actual != other {
                out.push(format!("{path}: {actual}, want {other}"));
            }
        }
    }
}

fn meters(v: Option<&Value>) -> Option<Meters> {
    v.filter(|u| !u.is_null()).map(|u| serde_json::from_value(u.clone()).expect("usage"))
}

/// Runs every scenario against a fresh harness each, returning the failures (empty when all pass).
pub async fn run_all<F, Fut>(make: F) -> Vec<String>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Harness>,
{
    let data = load();
    let scenarios = data["scenarios"].as_array().expect("scenarios");
    assert!(scenarios.len() >= 25, "only {} scenarios", scenarios.len());
    let mut failures = Vec::new();
    for scenario in scenarios {
        let h = make().await;
        for failure in run_scenario(&data, scenario, &h).await {
            failures.push(format!("[{}] {failure}", text(scenario, "name")));
        }
    }
    failures
}

async fn run_scenario(data: &Value, scenario: &Value, h: &Harness) -> Vec<String> {
    let mut fails: Vec<String> = Vec::new();
    let mut refs: HashMap<String, String> = HashMap::new();
    let behaviors = scenario.get("providers").unwrap_or(&data["providers"]);
    let mut mocks: BTreeMap<String, Arc<MockProvider>> = BTreeMap::new();
    let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
    for (name, behavior) in behaviors.as_object().expect("providers") {
        let mock = Arc::new(MockProvider::new(behavior.as_str().unwrap_or("")));
        providers.insert(name.clone(), mock.clone());
        mocks.insert(name.clone(), mock);
    }
    let router = Router::new(h.store.clone(), providers, DIGEST_KEY).expect("router").with_clock(h.clock.clone());
    let workspace = format!("ws-{}", Uuid::new_v4());
    let mut principals: HashMap<String, Principal> = HashMap::new();
    for (name, raw) in data["principals"].as_object().expect("principals") {
        let strings = |v: &Value| v.as_array().map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect::<Vec<_>>());
        principals.insert(
            name.clone(),
            Principal {
                workspace: workspace.clone(),
                environment: "test".into(),
                customer: text(raw, "customer").into(),
                scopes: raw.get("scopes").and_then(strings).unwrap_or_default(),
                service_ids: raw.get("service_ids").and_then(strings),
                ..Principal::default()
            },
        );
    }
    let who = |step: &Value| principals[Some(text(step, "principal")).filter(|n| !n.is_empty()).unwrap_or("owner")].clone();
    let service = |step: &Value| -> Result<Service, String> {
        let base = &data["services"][text(step, "service")];
        let doc = merge(base, step.get("patch").unwrap_or(&Value::Null));
        parse_service(&serde_json::to_vec(&doc).expect("service")).map_err(|e| format!("service {}: {e}", text(step, "service")))
    };

    for (index, step) in scenario["steps"].as_array().expect("steps").iter().enumerate() {
        let op = text(step, "op");
        let label = format!("#{index} {op}");
        let p = who(step);
        let mut result = Value::Null;
        let mut failure: Option<(String, u16)> = None;
        macro_rules! check {
            ($e:expr) => {
                match $e {
                    Ok(v) => Some(v),
                    Err(e) => {
                        failure = Some(match &e {
                            super_router_router::StoreError::Router(re) => (re.code.clone(), re.status),
                            other => (other.to_string(), 0),
                        });
                        None
                    }
                }
            };
        }
        match op {
            "publish" => match service(step) {
                Ok(s) => {
                    check!(h.store.publish(&p, &s, text(step, "mode"), None).await);
                }
                Err(e) => {
                    fails.push(format!("{label}: {e}"));
                    continue;
                }
            },
            "grant" => {
                let expires = step.get("expires_in_ms").and_then(Value::as_i64).map(|ms| (h.clock)() + ms);
                if let Some(id) = check!(
                    h.store.grant(&p, text(step, "kind"), text(step, "asset"), text(step, "amount"), text(step, "source"), expires).await
                ) {
                    result = Value::String(id);
                }
            }
            "infer" => {
                let doc = merge(&data["invocation"], step.get("input").unwrap_or(&Value::Null));
                let key = Some(text(step, "key")).filter(|k| !k.is_empty()).map_or_else(|| format!("key-{index}"), String::from);
                match router.infer(&p, &serde_json::to_vec(&doc).expect("invocation"), &key, None, CancellationToken::new()).await {
                    Ok(res) => {
                        result = serde_json::to_value(&res).expect("result");
                        if let Some(name) = step.get("as").and_then(Value::as_str) {
                            refs.insert(name.to_string(), res.id);
                        }
                    }
                    Err(e) => failure = Some((e.error.code, e.error.status)),
                }
            }
            "balance" => {
                if let Some(rows) = check!(h.store.balance(&p).await) {
                    result = json!(rows
                        .iter()
                        .map(|r| json!({"id": r.id, "kind": r.kind, "asset": r.asset, "balance": r.balance, "held": r.held, "available": r.available}))
                        .collect::<Vec<_>>());
                }
            }
            "buckets" => {
                let rows = (h.buckets)(p.clone()).await;
                result =
                    json!(rows.iter().map(|b| json!({"source": b.source, "remaining": b.remaining, "held": b.held})).collect::<Vec<_>>());
            }
            "get" => {
                if let Some(r) = check!(h.store.get(&p, refs.get(text(step, "request")).map_or("", String::as_str)).await) {
                    result = json!({"execution": r.execution, "billing": r.billing, "cancel_requested": r.cancel_requested, "output_started": r.output_started, "provider_spent": r.provider_spent});
                }
            }
            "cancel" => {
                check!(h.store.cancel(&p, refs.get(text(step, "request")).map_or("", String::as_str)).await);
            }
            "advance" => (h.advance)(step["ms"].as_i64().unwrap_or(0)).await,
            "disable" => {
                check!(h.store.disable(&p, text(step, "offering")).await);
            }
            "recover" => {
                if let Some(n) = check!(h.store.recover().await) {
                    result = json!(n);
                }
            }
            "provider_calls" => result = json!(mocks.get(text(step, "provider")).map_or(0, |m| m.calls())),
            "reserve" => {
                let svc = match service(step) {
                    Ok(s) => s,
                    Err(e) => {
                        fails.push(format!("{label}: {e}"));
                        continue;
                    }
                };
                if let Some(legs) = check!(h.store.accounts(&p, &svc).await) {
                    let reserved = step.get("reserved").and_then(Value::as_object).cloned().unwrap_or_else(Map::new);
                    let legs: Vec<Leg> = legs
                        .into_iter()
                        .map(|mut l| {
                            l.reserved = reserved.get(&l.kind).and_then(Value::as_str).unwrap_or("0").to_string();
                            l
                        })
                        .collect();
                    let digest = Some(text(step, "digest")).filter(|d| !d.is_empty()).unwrap_or("d");
                    let deadline = (h.clock)() + step["timeout_ms"].as_i64().unwrap_or(0);
                    if let Some(out) =
                        check!(h.store.reserve(&p, text(step, "key"), digest, &svc, &legs, deadline, &Decision::default()).await)
                    {
                        refs.insert(text(step, "as").to_string(), out.request.id.clone());
                        result = json!({"fresh": out.fresh, "execution": out.request.execution, "billing": out.request.billing});
                    }
                }
            }
            "begin" => {
                let svc = match service(step) {
                    Ok(s) => s,
                    Err(e) => {
                        fails.push(format!("{label}: {e}"));
                        continue;
                    }
                };
                let Some(offering) = svc.offerings.iter().find(|o| o.id == text(step, "offering")) else {
                    fails.push(format!("{label}: no offering {}", text(step, "offering")));
                    continue;
                };
                if let Some(id) = check!(
                    h.store.begin(&p, refs.get(text(step, "request")).map_or("", String::as_str), offering, text(step, "bound")).await
                ) {
                    refs.insert(text(step, "as").to_string(), id);
                }
            }
            "attempt" => {
                let usage = meters(step.get("usage"));
                let cost = step.get("cost").and_then(Value::as_str);
                check!(
                    h.store
                        .attempt_result(
                            &p,
                            refs.get(text(step, "request")).map_or("", String::as_str),
                            refs.get(text(step, "attempt")).map_or("", String::as_str),
                            text(step, "status"),
                            usage.as_ref(),
                            cost,
                            ""
                        )
                        .await
                );
            }
            "output" => {
                check!(h.store.output(&p, refs.get(text(step, "request")).map_or("", String::as_str)).await);
            }
            "finish" => {
                let usage = meters(step.get("usage"));
                let pending = step.get("pending").and_then(Value::as_bool).unwrap_or(false);
                check!(
                    h.store
                        .finish(
                            &p,
                            refs.get(text(step, "request")).map_or("", String::as_str),
                            text(step, "execution"),
                            usage.as_ref(),
                            pending
                        )
                        .await
                );
            }
            other => {
                fails.push(format!("{label}: unknown op {other}"));
                continue;
            }
        }
        if let Some(want) = step.get("error").filter(|e| e.is_object()) {
            match &failure {
                None => fails.push(format!("{label}: succeeded, want error {}", want["code"])),
                Some((code, status)) => {
                    if want["code"].as_str() != Some(code) {
                        fails.push(format!("{label}: error {code}, want {}", want["code"]));
                    }
                    if let Some(s) = want.get("status").and_then(Value::as_u64) {
                        if u64::from(*status) != s {
                            fails.push(format!("{label}: status {status}, want {s}"));
                        }
                    }
                }
            }
            continue;
        }
        if let Some((code, status)) = failure {
            fails.push(format!("{label}: unexpected error {code} ({status})"));
            continue;
        }
        if let Some(expect) = step.get("expect") {
            matches(&result, expect, &refs, &label, &mut fails);
        }
    }
    fails
}

/// Fires more simultaneous requests than the balance can hold and checks the invariants that must survive any
/// interleaving: no overspend, no lost update, nothing left held, and every refusal is a 402.
pub async fn concurrency(h: Harness) -> Result<(), String> {
    let data = load();
    let service = parse_service(&serde_json::to_vec(&data["services"]["chat"]).unwrap()).map_err(|e| e.to_string())?;
    let p = Principal {
        workspace: format!("ws-{}", Uuid::new_v4()),
        environment: "test".into(),
        customer: "c1".into(),
        scopes: ["invoke", "service:write", "billing:write", "billing:read"].map(String::from).to_vec(),
        ..Principal::default()
    };
    h.store.publish(&p, &service, "", None).await.map_err(|e| e.to_string())?;
    // One request reserves 1024*2 + 32*3 = 2144 atoms and costs 35 once settled. Room for ten holds.
    const HOLD: u64 = 2144;
    const COST: u64 = 35;
    const WORKERS: usize = 24;
    h.store.grant(&p, "customer_sale", "credit:demo", &(HOLD * 10).to_string(), "g1", None).await.map_err(|e| e.to_string())?;
    let mock = Arc::new(MockProvider::new("success").with_delay(std::time::Duration::from_millis(400)));
    let providers: HashMap<String, Arc<dyn Provider>> = HashMap::from([("ok".to_string(), mock.clone() as Arc<dyn Provider>)]);
    let router = Arc::new(Router::new(h.store.clone(), providers, DIGEST_KEY)?.with_clock(h.clock.clone()));
    let body = Arc::new(serde_json::to_vec(&data["invocation"]).unwrap());
    let mut tasks = Vec::new();
    for i in 0..WORKERS {
        let (router, p, body) = (router.clone(), p.clone(), body.clone());
        tasks.push(tokio::spawn(async move { router.infer(&p, &body, &format!("key-{i}"), None, CancellationToken::new()).await }));
    }
    let (mut settled, mut refused) = (0u64, 0usize);
    for task in tasks {
        match task.await.map_err(|e| e.to_string())? {
            Ok(_) => settled += 1,
            Err(e) if e.error.code == "insufficient_balance" && e.error.status == 402 => refused += 1,
            Err(e) => return Err(format!("unexpected error: {}", e.error.code)),
        }
    }
    if settled < 10 || refused < 1 || settled as usize + refused != WORKERS {
        return Err(format!("{settled} settled and {refused} refused of {WORKERS}"));
    }
    if mock.calls() != settled {
        return Err(format!("provider called {} times for {settled} admitted requests", mock.calls()));
    }
    let rows = h.store.balance(&p).await.map_err(|e| e.to_string())?;
    let want = (HOLD * 10 - COST * settled).to_string();
    match rows.as_slice() {
        [row] if row.balance == want && row.held == "0" => Ok(()),
        other => Err(format!("balance {other:?}, want {want} held 0")),
    }
}
