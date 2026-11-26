//! Runs `conformance/failure.v1.json`: an upstream refusal (status + message) to its class and provider error.

use std::path::Path;

use serde_json::Value;
use super_router_contracts::classify_failure;

#[test]
fn failure_classification_matches_the_shared_cases() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../conformance/failure.v1.json");
    let data: Value = serde_json::from_slice(&std::fs::read(path).expect("read failure.v1.json")).expect("parse failure.v1.json");
    let cases = data["cases"].as_array().unwrap();
    assert!(cases.len() >= 40, "only {} cases", cases.len());
    let mut failures = vec![];
    for c in cases {
        let name = c["name"].as_str().unwrap();
        let status = c["status"].as_u64().unwrap() as u16;
        let class = classify_failure(status, c["message"].as_str().unwrap());
        let e = class.provider_error(status);
        let want = &c["expect"];
        let have = (class.name(), e.definitive, e.retryable, e.code.as_str(), class.is_offering_scoped());
        let want = (
            want["class"].as_str().unwrap(),
            want["definitive"].as_bool().unwrap(),
            want["retryable"].as_bool().unwrap(),
            want["code"].as_str().unwrap(),
            want["offering_scoped"].as_bool().unwrap(),
        );
        if have != want {
            failures.push(format!("{name}: have {have:?} want {want:?}"));
        }
    }
    assert!(failures.is_empty(), "{} cases differ:\n{}", failures.len(), failures.join("\n"));
}
