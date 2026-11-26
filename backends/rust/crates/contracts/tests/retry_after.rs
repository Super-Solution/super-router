//! Runs `conformance/retry_after.v1.json`: the value of a `Retry-After` header to milliseconds.

use std::path::Path;

use serde_json::Value;
use super_router_contracts::{parse_retry_after, RETRY_AFTER_MAX_MS};

#[test]
fn headers_read_as_the_shared_cases_say() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../conformance/retry_after.v1.json");
    let data: Value = serde_json::from_slice(&std::fs::read(path).expect("read retry_after.v1.json")).expect("parse retry_after.v1.json");
    let cases = data["cases"].as_array().unwrap();
    assert!(cases.len() >= 30, "only {} cases", cases.len());
    let mut failures = vec![];
    for c in cases {
        let header = c["header"].as_str().unwrap();
        let want = c["expect_ms"].as_u64();
        let have = parse_retry_after(header);
        if have != want {
            failures.push(format!("{header:?}: have {have:?} want {want:?}"));
        }
    }
    assert!(failures.is_empty(), "{} headers differ:\n{}", failures.len(), failures.join("\n"));
}

#[test]
fn the_cap_is_one_day() {
    assert_eq!(RETRY_AFTER_MAX_MS, 24 * 60 * 60 * 1000);
    assert_eq!(parse_retry_after("86400"), Some(RETRY_AFTER_MAX_MS));
    assert_eq!(parse_retry_after("86401"), Some(RETRY_AFTER_MAX_MS));
}
