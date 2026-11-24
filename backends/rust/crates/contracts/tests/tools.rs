//! Runs the `invocation` section of `conformance/tools.v1.json`: tool declarations, tool calls, tool results,
//! image parts and several system messages, parsed the way the TypeScript reference parses them.

use std::path::Path;

use serde_json::{Number, Value};
use super_router_contracts::{canonical, invocation_needs, js_stringify, js_trim, parse_invocation, Needs};

fn load() -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../conformance/tools.v1.json");
    serde_json::from_slice(&std::fs::read(path).expect("read tools.v1.json")).expect("parse tools.v1.json")
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
fn tool_invocations_match_the_reference() {
    let data = load();
    let cases = data["invocation"].as_array().unwrap();
    assert!(cases.len() >= 80, "only {} cases", cases.len());
    let mut failures = vec![];
    for c in cases {
        let name = c["name"].as_str().unwrap();
        let raw = match c.get("raw").and_then(Value::as_str) {
            Some(raw) => raw.as_bytes().to_vec(),
            None => serde_json::to_vec(&c["input"]).unwrap(),
        };
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
                let want = c["expect"]["canonical"].as_str().unwrap();
                let have = canonical(&i).unwrap();
                if have != want {
                    failures.push(format!("{name}: canonical\n have {have}\n want {want}"));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{} cases differ:\n{}", failures.len(), failures.join("\n"));
}

#[test]
fn needs_follow_what_the_invocation_carries() {
    let parse = |input: &str| {
        let raw = format!(r#"{{"service":"s","customer_id":"c","input":{input},"limits":{{}}}}"#);
        parse_invocation(raw.as_bytes()).unwrap()
    };
    let cases = [
        (r#"{"messages":[{"role":"user","content":"hi"}]}"#, Needs::default()),
        (
            r#"{"messages":[{"role":"user","content":"hi"}],"tools":[{"name":"a","input_schema":{"type":"object"}}]}"#,
            Needs { tools: true, images: false },
        ),
        (
            r#"{"messages":[{"role":"user","content":"hi"},{"role":"assistant","tool_calls":[{"id":"a","name":"a","arguments":"{}"}]},{"role":"tool","tool_call_id":"a","content":""}]}"#,
            Needs { tools: true, images: false },
        ),
        (
            r#"{"messages":[{"role":"user","content":[{"type":"image","url":"https://e.example/a.png"}]}]}"#,
            Needs { tools: false, images: true },
        ),
        (r#"{"messages":[{"role":"user","content":[{"type":"text","text":"x"}]}]}"#, Needs::default()),
    ];
    for (input, want) in cases {
        assert_eq!(invocation_needs(&parse(input)), want, "{input}");
    }
}

#[test]
fn trim_uses_the_javascript_whitespace_set() {
    assert_eq!(js_trim("\u{feff}\u{3000} {} \n\u{2028}"), "{}");
    assert_eq!(js_trim("\u{85}{}"), "\u{85}{}", "NEL is not JavaScript whitespace");
    assert_eq!(js_trim(" \t "), "");
}

#[test]
fn stringify_keeps_key_order_and_renders_numbers_like_javascript() {
    let cases = [
        (
            r#"{"b":1,"a":[1.0,1e21,1E-7,-0,0.000001,12345678901234567890]}"#,
            r#"{"b":1,"a":[1,1e+21,1e-7,0,0.000001,12345678901234567000]}"#,
        ),
        (r#"{"a":1,"a":2,"b":"é 😀"}"#, "{\"a\":2,\"b\":\"\u{e9}\u{2028}\u{1f600}\"}"),
        (r#" { "x" : { } , "y" : [ ] } "#, r#"{"x":{},"y":[]}"#),
        (r#"{"b":1,"10":2,"2":3,"01":4,"4294967295":5,"4294967294":6}"#, r#"{"2":3,"10":2,"4294967294":6,"b":1,"01":4,"4294967295":5}"#),
    ];
    for (input, want) in cases {
        assert_eq!(js_stringify(input).unwrap(), want, "{input}");
    }
}

#[test]
fn a_call_signature_is_kept_and_a_bad_one_is_refused() {
    let with = |signature: &str| {
        let raw = format!(
            r#"{{"service":"s","customer_id":"c","input":{{"messages":[{{"role":"user","content":"hi"}},{{"role":"assistant","tool_calls":[{{"id":"c1","name":"lookup","arguments":"{{}}"{signature}}}]}},{{"role":"tool","tool_call_id":"c1","content":"x"}}],"tools":[{{"name":"lookup","input_schema":{{"type":"object"}}}}]}},"limits":{{}}}}"#
        );
        parse_invocation(raw.as_bytes())
    };
    let kept = with(r#","signature":"c2lnLUE9""#).expect("a signed call parses");
    let calls = kept.input.messages[1].tool_calls.as_ref().unwrap();
    assert_eq!(calls[0].signature.as_deref(), Some("c2lnLUE9"));
    assert!(canonical(&kept).unwrap().contains(r#""signature":"c2lnLUE9""#), "the digest must cover it");
    assert_eq!(with("").unwrap().input.messages[1].tool_calls.as_ref().unwrap()[0].signature, None);
    let too_long = format!(r#","signature":"{}""#, "a".repeat(16_385));
    for bad in [
        r#","signature":"""#,
        r#","signature":"a b""#,
        r#","signature":"a\nb""#,
        r#","signature":5"#,
        r#","signature":null"#,
        too_long.as_str(),
    ] {
        assert_eq!(with(bad).err().map(|e| e.code), Some("invalid_request".to_string()), "{bad}");
    }
}
