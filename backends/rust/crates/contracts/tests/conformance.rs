//! Runs `conformance/contracts.v1.json`: parsing untrusted JSON into a service or an invocation.

use std::path::Path;

use serde_json::{Number, Value};
use super_router_contracts::{canonical, parse_invocation, parse_service};

fn load() -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../conformance/contracts.v1.json");
    serde_json::from_slice(&std::fs::read(path).expect("read contracts.v1.json")).expect("parse contracts.v1.json")
}

/// The reference compares generic JSON, where every number is a double.
fn normalise(v: Value) -> Value {
    match v {
        Value::Number(n) => Value::Number(Number::from_f64(n.as_f64().unwrap()).unwrap()),
        Value::Array(a) => Value::Array(a.into_iter().map(normalise).collect()),
        Value::Object(m) => Value::Object(m.into_iter().map(|(k, v)| (k, normalise(v))).collect()),
        other => other,
    }
}

#[test]
fn service_parsing_matches_the_reference() {
    let data = load();
    let cases = data["service"].as_array().unwrap();
    assert!(cases.len() >= 50, "only {} service cases", cases.len());
    let mut failures = vec![];
    for c in cases {
        let name = c["name"].as_str().unwrap();
        let raw = serde_json::to_vec(&c["input"]).unwrap();
        match (parse_service(&raw), c["expect"].get("error")) {
            (Err(e), Some(code)) if e.code == code.as_str().unwrap() && e.status == 400 => {}
            (Err(e), want) => failures.push(format!("{name}: error {} want {want:?}", e.code)),
            (Ok(_), Some(code)) => failures.push(format!("{name}: accepted, want {code}")),
            (Ok(s), None) => {
                let have = normalise(serde_json::to_value(&s).unwrap());
                let want = normalise(c["expect"]["ok"].clone());
                if have != want {
                    failures.push(format!("{name}:\n have {have}\n want {want}"));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{} service cases differ:\n{}", failures.len(), failures.join("\n"));
}

#[test]
fn invocation_parsing_matches_the_reference() {
    let data = load();
    let cases = data["invocation"].as_array().unwrap();
    assert!(cases.len() >= 30, "only {} invocation cases", cases.len());
    let mut failures = vec![];
    for c in cases {
        let name = c["name"].as_str().unwrap();
        let raw = serde_json::to_vec(&c["input"]).unwrap();
        match (parse_invocation(&raw), c["expect"].get("error")) {
            (Err(e), Some(code)) if e.code == code.as_str().unwrap() && e.status == 400 => {}
            (Err(e), want) => failures.push(format!("{name}: error {} want {want:?}", e.code)),
            (Ok(_), Some(code)) => failures.push(format!("{name}: accepted, want {code}")),
            (Ok(i), None) => {
                let have = normalise(serde_json::to_value(&i).unwrap());
                let want = normalise(c["expect"]["ok"].clone());
                if have != want {
                    failures.push(format!("{name}:\n have {have}\n want {want}"));
                }
                if let Some(want) = c["expect"].get("canonical").and_then(Value::as_str) {
                    let have = canonical(&i).unwrap();
                    if have != want {
                        failures.push(format!("{name}: canonical\n have {have}\n want {want}"));
                    }
                }
            }
        }
    }
    assert!(failures.is_empty(), "{} invocation cases differ:\n{}", failures.len(), failures.join("\n"));
}

/// A deterministic xorshift generator, so a failure reproduces from its seed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn mutate(rng: &mut Rng, mut data: Vec<u8>) -> Vec<u8> {
    for _ in 0..=rng.below(4) {
        if data.is_empty() {
            break;
        }
        let at = rng.below(data.len());
        match rng.below(5) {
            0 => data[at] = rng.next() as u8,
            1 => {
                let end = (at + 1 + rng.below(16)).min(data.len());
                data.drain(at..end);
            }
            2 => data.insert(at, b"{}[]\",:0-.e\\ntfu"[rng.below(16)]),
            3 => {
                let end = (at + 1 + rng.below(32)).min(data.len());
                let copy = data[at..end].to_vec();
                data.splice(at..at, copy);
            }
            _ => data.truncate(at),
        }
    }
    data
}

/// Parsing untrusted input must never panic, and an accepted invocation must have a canonical form.
#[test]
fn parsing_survives_mutated_inputs() {
    let data = load();
    let seeds: Vec<Vec<u8>> = ["service", "invocation"]
        .iter()
        .flat_map(|k| data[*k].as_array().unwrap().iter().map(|c| serde_json::to_vec(&c["input"]).unwrap()))
        .collect();
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut accepted = 0;
    for _ in 0..40_000 {
        let seed = seeds[rng.below(seeds.len())].clone();
        let input = mutate(&mut rng, seed);
        if let Ok(i) = parse_invocation(&input) {
            accepted += 1;
            canonical(&i).expect("an accepted invocation has a canonical form");
        }
        let _ = parse_service(&input);
    }
    assert!(accepted > 0, "the mutations never produced a valid invocation, so the check proves little");
}
