//! Runs `conformance/pricing.v1.json` against the Rust pricing core.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;
use super_router_pricing::{bound, charge, compile_credit_price, Price, RateCard, TokenCounts, Usage, MEDIA_KEYS};

fn load() -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../conformance/pricing.v1.json");
    serde_json::from_slice(&std::fs::read(path).expect("read pricing.v1.json")).expect("parse pricing.v1.json")
}

fn price(v: &Value) -> Price {
    serde_json::from_value(v.clone()).expect("price")
}

fn int(v: &Value, key: &str) -> i64 {
    v.get(key).map_or(0, |n| n.as_i64().unwrap_or_else(|| panic!("{key} is not an integer")))
}

fn usage(v: &Value) -> Usage {
    let mut u = Usage {
        input: int(v, "input"),
        cached: int(v, "cached"),
        cache_write: int(v, "cache_write"),
        output: int(v, "output"),
        ..Usage::default()
    };
    for key in MEDIA_KEYS {
        u.media.insert(key.to_string(), int(v, key));
    }
    if let Some(units) = v.get("units").and_then(Value::as_object) {
        u.units = units.iter().map(|(k, n)| (k.clone(), n.as_i64().expect("unit"))).collect::<BTreeMap<_, _>>();
    }
    u
}

#[test]
fn charge_matches_the_reference() {
    let data = load();
    let cases = data["charge"].as_array().unwrap();
    assert!(cases.len() >= 100, "only {} charge cases", cases.len());
    for (i, c) in cases.iter().enumerate() {
        let got = charge(&usage(&c["usage"]), &price(&c["price"]));
        match (c["expect"].get("error"), c["expect"].get("ok")) {
            (Some(code), _) => assert_eq!(got.unwrap_err().code, code.as_str().unwrap(), "charge[{i}]"),
            (None, Some(ok)) => assert_eq!(got.unwrap().to_string(), ok.as_str().unwrap(), "charge[{i}]"),
            _ => panic!("charge[{i}]: no expectation"),
        }
    }
}

#[test]
fn bound_matches_the_reference() {
    let data = load();
    let cases = data["bound"].as_array().unwrap();
    assert!(cases.len() >= 50);
    for (i, c) in cases.iter().enumerate() {
        let got = bound(int(c, "input"), int(c, "output"), &price(&c["price"])).unwrap();
        assert_eq!(got.to_string(), c["expect"]["ok"].as_str().unwrap(), "bound[{i}]");
    }
}

#[test]
fn compile_credit_price_matches_the_reference() {
    let data = load();
    let cases = data["compile_credit_price"].as_array().unwrap();
    let mut errors = 0;
    for (i, c) in cases.iter().enumerate() {
        let got = compile_credit_price(&c["config"]);
        if let Some(code) = c["expect"].get("error") {
            errors += 1;
            assert_eq!(got.unwrap_err().code, code.as_str().unwrap(), "compile[{i}]");
            continue;
        }
        let got = got.unwrap_or_else(|e| panic!("compile[{i}]: {e}"));
        assert_eq!(serde_json::to_value(got).unwrap(), c["expect"]["ok"], "compile[{i}]");
    }
    assert!(errors >= 10, "only {errors} error cases");
}

fn card(v: &Value) -> RateCard {
    serde_json::from_value(v.clone()).expect("rate card")
}

#[test]
fn rate_card_charge_matches_the_oracle() {
    let data = load();
    let cases = data["rate_card_charge"].as_array().unwrap();
    let mut errors = 0;
    for (i, c) in cases.iter().enumerate() {
        let counts: TokenCounts = serde_json::from_value(c["usage"].clone()).expect("counts");
        let got = card(&c["card"]).charge(&counts);
        match (c["expect"].get("error"), c["expect"].get("ok")) {
            (Some(code), _) => {
                errors += 1;
                assert_eq!(got.unwrap_err().code, code.as_str().unwrap(), "rate_card_charge[{i}]");
            }
            (None, Some(ok)) => assert_eq!(got.unwrap().to_string(), ok.as_str().unwrap(), "rate_card_charge[{i}]"),
            _ => panic!("rate_card_charge[{i}]: no expectation"),
        }
    }
    assert!(cases.len() >= 30 && errors >= 10, "{} cases, {errors} errors", cases.len());
}

#[test]
fn rate_card_bound_matches_the_oracle() {
    let data = load();
    let cases = data["rate_card_bound"].as_array().unwrap();
    let mut errors = 0;
    for (i, c) in cases.iter().enumerate() {
        let got = card(&c["card"]).bound(int(c, "input"), int(c, "output"));
        match (c["expect"].get("error"), c["expect"].get("ok")) {
            (Some(code), _) => {
                errors += 1;
                assert_eq!(got.unwrap_err().code, code.as_str().unwrap(), "rate_card_bound[{i}]");
            }
            (None, Some(ok)) => assert_eq!(got.unwrap().to_string(), ok.as_str().unwrap(), "rate_card_bound[{i}]"),
            _ => panic!("rate_card_bound[{i}]: no expectation"),
        }
    }
    assert!(cases.len() >= 10 && errors >= 4, "{} cases, {errors} errors", cases.len());
}
