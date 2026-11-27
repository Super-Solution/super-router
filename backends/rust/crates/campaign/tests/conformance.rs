//! The pure parts against `conformance/campaign.v1.json`, which was produced by an independent implementation of the
//! written rules, so a port in another language can read the same file.

use serde_json::Value;
use super_router_campaign::{codes, rebate, signals};

fn data() -> Value {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../conformance/campaign.v1.json");
    serde_json::from_str(&std::fs::read_to_string(path).expect("conformance file")).expect("valid json")
}

fn hex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

fn opt(v: &Value) -> Option<String> {
    v.as_str().map(str::to_owned)
}

#[test]
fn serial_and_invite_generation() {
    let d = data();
    for c in d["generate_serial"].as_array().unwrap() {
        assert_eq!(codes::generate_serial(&hex(c["random"].as_str().unwrap())), c["serial"].as_str().unwrap());
    }
    for c in d["generate_invite"].as_array().unwrap() {
        assert_eq!(codes::generate_invite(&hex(c["random"].as_str().unwrap())), c["invite"].as_str().unwrap());
    }
}

#[test]
fn typing_and_normalising() {
    let d = data();
    for c in d["normalize_serial"].as_array().unwrap() {
        assert_eq!(codes::normalize_serial(c["input"].as_str().unwrap()), opt(&c["serial"]), "{c}");
    }
    for c in d["normalize_custom"].as_array().unwrap() {
        assert_eq!(codes::normalize_custom(c["input"].as_str().unwrap()), opt(&c["custom"]), "{c}");
    }
}

#[test]
fn keyed_hashes() {
    let d = data();
    let key = d["hmac_key"].as_str().unwrap().as_bytes();
    for c in d["hash"].as_array().unwrap() {
        let got: String =
            codes::hash(key, c["kind"].as_str().unwrap(), c["canonical"].as_str().unwrap()).iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(got, c["hmac_sha256_hex"].as_str().unwrap());
    }
}

#[test]
fn rebates_and_signals() {
    let d = data();
    for c in d["rebate"].as_array().unwrap() {
        assert_eq!(rebate::rebate(c["base"].as_i64().unwrap(), c["bps"].as_i64().unwrap() as i32), c["rebate"].as_i64().unwrap(), "{c}");
    }
    for c in d["normalize_email"].as_array().unwrap() {
        assert_eq!(signals::normalize_email(c["input"].as_str().unwrap()), opt(&c["email"]), "{c}");
    }
    for c in d["network_prefix"].as_array().unwrap() {
        assert_eq!(signals::network_prefix(c["input"].as_str().unwrap()), opt(&c["network"]), "{c}");
    }
}
