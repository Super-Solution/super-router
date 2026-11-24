//! Replays `conformance/health.v1.json`: scripted incidents against the cooldown book.

use std::path::Path;

use serde_json::Value;
use super_router_contracts::FailureClass;
use super_router_routing::{Admit, HealthBook, HealthPolicy};

fn policy(v: Option<&Value>) -> HealthPolicy {
    let mut p = HealthPolicy::default();
    let Some(v) = v else { return p };
    let get = |k: &str| v.get(k).and_then(Value::as_i64);
    p.rate_limited_ms = get("rate_limited_ms").unwrap_or(p.rate_limited_ms);
    p.quota_exhausted_ms = get("quota_exhausted_ms").unwrap_or(p.quota_exhausted_ms);
    p.credentials_rejected_ms = get("credentials_rejected_ms").unwrap_or(p.credentials_rejected_ms);
    p.model_unavailable_ms = get("model_unavailable_ms").unwrap_or(p.model_unavailable_ms);
    p.region_blocked_ms = get("region_blocked_ms").unwrap_or(p.region_blocked_ms);
    p.max_ms = get("max_ms").unwrap_or(p.max_ms);
    p.probe_lease_ms = get("probe_lease_ms").unwrap_or(p.probe_lease_ms);
    p
}

#[test]
fn incidents_match_the_shared_cases() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../conformance/health.v1.json");
    let data: Value = serde_json::from_slice(&std::fs::read(path).expect("read health.v1.json")).expect("parse health.v1.json");
    let cases = data["cases"].as_array().unwrap();
    assert!(cases.len() >= 12, "only {} cases", cases.len());
    let mut failures = vec![];
    for c in cases {
        let name = c["name"].as_str().unwrap();
        let mut book = HealthBook::new(policy(c.get("policy")));
        for (i, s) in c["steps"].as_array().unwrap().iter().enumerate() {
            let id = s["id"].as_str().unwrap();
            let at = s["at"].as_i64().unwrap();
            match s["op"].as_str().unwrap() {
                "admit" => {
                    let want = &s["expect"];
                    let have = match book.admit(id, at) {
                        Admit::Open => serde_json::json!({"result": "open"}),
                        Admit::Probe => serde_json::json!({"result": "probe"}),
                        Admit::Cooling { until_ms, class } => {
                            serde_json::json!({"result": "cooling", "until_ms": until_ms, "class": class.name()})
                        }
                    };
                    if &have != want {
                        failures.push(format!("{name} step {i}: have {have} want {want}"));
                    }
                }
                "fail" => {
                    let class = FailureClass::from_name(s["class"].as_str().unwrap()).expect("a class name");
                    book.failed(id, class, at);
                }
                "succeed" => book.succeeded(id),
                other => panic!("unknown op {other}"),
            }
        }
        let size = c["final_size"].as_u64().unwrap() as usize;
        if book.len() != size {
            failures.push(format!("{name}: {} records, want {size}", book.len()));
        }
    }
    assert!(failures.is_empty(), "{} steps differ:\n{}", failures.len(), failures.join("\n"));
}
