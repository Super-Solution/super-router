//! Runs `conformance/routing.v1.json` against `route_at`.

use std::path::Path;

use serde_json::Value;
use super_router_contracts::{Invocation, Service};
use super_router_routing::route_at;

#[test]
fn route_matches_the_reference() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../conformance/routing.v1.json");
    let data: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let cases = data["route"].as_array().unwrap();
    assert!(cases.len() >= 300, "only {} route cases", cases.len());
    let (mut with_candidates, mut errors) = (0, 0);
    for (i, c) in cases.iter().enumerate() {
        let service: Service = serde_json::from_value(c["service"].clone()).unwrap_or_else(|e| panic!("route[{i}] service: {e}"));
        // The fixtures leave out the identity fields that routing never reads.
        let mut raw = c["invocation"].clone();
        for (key, fill) in [("service", "svc0"), ("customer_id", "c1")] {
            raw.as_object_mut().unwrap().entry(key).or_insert_with(|| Value::from(fill));
        }
        let invocation: Invocation = serde_json::from_value(raw).unwrap_or_else(|e| panic!("route[{i}] invocation: {e}"));
        let now = c.get("now").and_then(Value::as_i64).unwrap_or(1_700_000_000_000);
        let got = route_at(&service, &invocation, now);
        if let Some(code) = c["expect"].get("error") {
            errors += 1;
            assert_eq!(got.unwrap_err().code, code.as_str().unwrap(), "route[{i}]");
            continue;
        }
        let (candidates, excluded) = got.unwrap_or_else(|e| panic!("route[{i}]: {e}"));
        let want = &c["expect"]["ok"];
        let want_candidates = want["candidates"].as_array().unwrap();
        assert_eq!(candidates.len(), want_candidates.len(), "route[{i}] candidates");
        with_candidates += usize::from(!candidates.is_empty());
        for (j, (got, want)) in candidates.iter().zip(want_candidates).enumerate() {
            assert_eq!(got.offering.id, want["id"].as_str().unwrap(), "route[{i}][{j}] id");
            assert_eq!(got.cost.to_string(), want["cost"].as_str().unwrap(), "route[{i}][{j}] cost");
            assert_eq!(got.score, want["score"].as_f64().unwrap(), "route[{i}][{j}] score");
            assert_eq!(got.downgrade, want["downgrade"].as_bool().unwrap_or(false), "route[{i}][{j}] downgrade");
        }
        let want_excluded: Vec<(String, String)> = want["excluded"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| (e["id"].as_str().unwrap().into(), e["reason"].as_str().unwrap().into()))
            .collect();
        let got_excluded: Vec<(String, String)> = excluded.into_iter().map(|e| (e.id, e.reason)).collect();
        assert_eq!(got_excluded, want_excluded, "route[{i}] excluded");
    }
    assert!(with_candidates >= 100 && errors >= 1, "weak coverage: {with_candidates} with candidates, {errors} errors");
}
