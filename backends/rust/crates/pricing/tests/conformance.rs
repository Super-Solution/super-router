//! Runs `conformance/pricing.v1.json` against the Rust pricing core.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;
use super_router_pricing::{bound, charge, compile_credit_price, Price, RateCard, RateSchedule, RateTiers, TokenCounts, Usage, MEDIA_KEYS};

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

/// A schedule as a case writes it. A case with an invalid schedule expects `invalid_rate_schedule` from decoding.
fn schedule(v: &Value) -> Result<RateSchedule, super_router_pricing::RouterError> {
    RateSchedule::from_json(&v.to_string())
}

#[test]
fn rate_schedule_charge_matches_the_oracle() {
    let data = load();
    let cases = data["rate_schedule_charge"].as_array().unwrap();
    let mut errors = 0;
    for (i, c) in cases.iter().enumerate() {
        let counts: TokenCounts = serde_json::from_value(c["usage"].clone()).expect("counts");
        let got = schedule(&c["schedule"]).and_then(|s| s.card_at(int(c, "at_ms")).and_then(|card| card.charge(&counts)));
        match (c["expect"].get("error"), c["expect"].get("ok")) {
            (Some(code), _) => {
                errors += 1;
                assert_eq!(got.unwrap_err().code, code.as_str().unwrap(), "rate_schedule_charge[{i}]");
            }
            (None, Some(ok)) => assert_eq!(got.unwrap().to_string(), ok.as_str().unwrap(), "rate_schedule_charge[{i}]"),
            _ => panic!("rate_schedule_charge[{i}]: no expectation"),
        }
    }
    assert!(cases.len() >= 12 && errors >= 6, "{} cases, {errors} errors", cases.len());
}

#[test]
fn rate_schedule_bound_matches_the_oracle() {
    let data = load();
    let cases = data["rate_schedule_bound"].as_array().unwrap();
    let mut errors = 0;
    for (i, c) in cases.iter().enumerate() {
        let got = schedule(&c["schedule"]).unwrap().bound_between(int(c, "start_ms"), int(c, "end_ms"), int(c, "input"), int(c, "output"));
        match (c["expect"].get("error"), c["expect"].get("ok")) {
            (Some(code), _) => {
                errors += 1;
                assert_eq!(got.unwrap_err().code, code.as_str().unwrap(), "rate_schedule_bound[{i}]");
            }
            (None, Some(ok)) => assert_eq!(got.unwrap().to_string(), ok.as_str().unwrap(), "rate_schedule_bound[{i}]"),
            _ => panic!("rate_schedule_bound[{i}]: no expectation"),
        }
    }
    assert!(cases.len() >= 8 && errors >= 2, "{} cases, {errors} errors", cases.len());
}

#[test]
fn rate_card_defaulted_matches_the_oracle() {
    let data = load();
    let cases = data["rate_card_defaulted"].as_array().unwrap();
    assert!(cases.len() >= 4);
    let names = |kinds: Vec<super_router_pricing::TokenKind>| kinds.into_iter().map(|k| k.as_str().to_string()).collect::<Vec<_>>();
    let list = |v: &Value| v.as_array().unwrap().iter().map(|k| k.as_str().unwrap().to_string()).collect::<Vec<_>>();
    for (i, c) in cases.iter().enumerate() {
        let card = card(&c["card"]);
        let counts: TokenCounts = serde_json::from_value(c["usage"].clone()).expect("counts");
        assert_eq!(names(card.defaulted()), list(&c["expect"]["all"]), "rate_card_defaulted[{i}] all");
        assert_eq!(names(card.defaulted_in(&counts)), list(&c["expect"]["used"]), "rate_card_defaulted[{i}] used");
    }
}

#[test]
fn rate_tiers_charge_matches_the_oracle() {
    let data = load();
    let cases = data["rate_tiers_charge"].as_array().unwrap();
    let mut errors = 0;
    for (i, c) in cases.iter().enumerate() {
        let counts: TokenCounts = serde_json::from_value(c["usage"].clone()).expect("counts");
        let got = RateTiers::from_json(&c["tiers"].to_string()).and_then(|t| t.charge(&counts));
        match (c["expect"].get("error"), c["expect"].get("ok")) {
            (Some(code), _) => {
                errors += 1;
                assert_eq!(got.unwrap_err().code, code.as_str().unwrap(), "rate_tiers_charge[{i}]");
            }
            (None, Some(ok)) => assert_eq!(got.unwrap().to_string(), ok.as_str().unwrap(), "rate_tiers_charge[{i}]"),
            _ => panic!("rate_tiers_charge[{i}]: no expectation"),
        }
    }
    assert!(cases.len() >= 15 && errors >= 8, "{} cases, {errors} errors", cases.len());
}

#[test]
fn rate_tiers_bound_matches_the_oracle() {
    let data = load();
    let cases = data["rate_tiers_bound"].as_array().unwrap();
    let mut errors = 0;
    for (i, c) in cases.iter().enumerate() {
        let got = RateTiers::from_json(&c["tiers"].to_string()).unwrap().bound(int(c, "input"), int(c, "output"));
        match (c["expect"].get("error"), c["expect"].get("ok")) {
            (Some(code), _) => {
                errors += 1;
                assert_eq!(got.unwrap_err().code, code.as_str().unwrap(), "rate_tiers_bound[{i}]");
            }
            (None, Some(ok)) => assert_eq!(got.unwrap().to_string(), ok.as_str().unwrap(), "rate_tiers_bound[{i}]"),
            _ => panic!("rate_tiers_bound[{i}]: no expectation"),
        }
    }
    assert!(cases.len() >= 10 && errors >= 2, "{} cases, {errors} errors", cases.len());
}
