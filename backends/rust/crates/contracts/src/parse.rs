//! Strict parsing of untrusted JSON into a [`Service`] or an [`Invocation`], and canonical JSON.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use serde::Serialize;
use serde_json::Value;
use super_router_pricing::{valid_atoms, valid_id, valid_unit_name, Price};

use crate::datetime::valid_datetime;
use crate::types::{
    is_media, kind_meter, safe_non_neg, Invocation, MeterLimits, Offering, RouterError, Sampling, Service, MAX_OUTPUT_TOKENS, MAX_SAFE,
    MAX_TIMEOUT_MS,
};

/// The error every `parse_*` function returns for input the reference would reject.
pub fn invalid_request() -> RouterError {
    RouterError::new("invalid_request", 400)
}

/// String limits count code points, as the reference's do.
fn length(s: &str) -> usize {
    s.chars().count()
}

fn in_range(s: &str, min: usize, max: usize) -> bool {
    (min..=max).contains(&length(s))
}

/// Reads one JSON document: no trailing data and no `null` anywhere (a `schema` object may hold any JSON).
fn decode_tree(data: &[u8]) -> Option<Value> {
    let tree: Value = serde_json::from_slice(data).ok()?;
    (tree.is_object() && !has_null(&tree)).then_some(tree)
}

fn has_null(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::Array(items) => items.iter().any(has_null),
        // A JSON Schema (`response_format`'s `schema`, a tool's `input_schema`) may hold null.
        Value::Object(m) => m.iter().any(|(k, e)| !((k == "schema" || k == "input_schema") && e.is_object()) && has_null(e)),
        _ => false,
    }
}

fn valid_price(p: &Price) -> bool {
    let per_ok = |per: i64| (1..=1_000_000_000).contains(&per);
    valid_id(&p.asset)
        && per_ok(p.per)
        && [&p.input, &p.cached, &p.cache_write, &p.output].into_iter().all(|a| valid_atoms(a))
        && p.media.iter().all(|(k, r)| is_media(k) && valid_atoms(&r.atoms) && per_ok(r.per))
        && p.units.iter().all(|(k, r)| valid_unit_name(k) && valid_atoms(&r.atoms) && per_ok(r.per) && in_range(&r.display_unit, 1, 80))
}

fn valid_meter_limits(m: &MeterLimits) -> bool {
    m.iter().all(|(k, n)| (is_media(k) || valid_unit_name(k)) && safe_non_neg(*n))
}

fn valid_offering(o: &Offering) -> bool {
    if ![&o.id, &o.provider, &o.model, &o.upstream_model, &o.region].into_iter().all(|s| valid_id(s)) {
        return false;
    }
    if o.task_kind.as_deref().is_some_and(|k| kind_meter(k).is_none()) || !valid_meter_limits(&o.max_usage) {
        return false;
    }
    if !o.labels.iter().all(|l| valid_id(l)) {
        return false;
    }
    if o.capabilities.iter().flatten().any(|c| c != "tools" && c != "structured_output") {
        return false;
    }
    if let Some(q) = &o.quality_metadata {
        if !in_range(&q.source, 1, 2048) || !in_range(&q.task, 1, 120) || !valid_datetime(&q.updated_at) {
            return false;
        }
    }
    for (label, evidence) in o.label_evidence.iter().flatten() {
        let verification_ok = evidence.verification == "verified" || evidence.verification == "unverified";
        if !valid_id(label)
            || !in_range(&evidence.source, 1, 2048)
            || !verification_ok
            || evidence.expires_at.as_deref().is_some_and(|e| !valid_datetime(e))
        {
            return false;
        }
    }
    if o.source != "platform" && o.source != "byok" {
        return false;
    }
    if !(1..=1_000_000).contains(&o.max_input_tokens) || !(1..=MAX_OUTPUT_TOKENS).contains(&o.max_output_tokens) || !valid_price(&o.price) {
        return false;
    }
    if o.latency_ms.is_some_and(|l| l.partial_cmp(&0.0).is_none_or(|c| c.is_lt())) {
        return false;
    }
    [o.reliability, o.quality].into_iter().flatten().all(|f| (0.0..=1.0).contains(&f))
}

fn valid_service(s: &Service) -> bool {
    if !valid_id(&s.id) || !(1..=MAX_SAFE).contains(&s.version) {
        return false;
    }
    if s.task_kind.as_deref().is_some_and(|k| kind_meter(k).is_none()) {
        return false;
    }
    match s.mode.as_str() {
        "specific" if s.model.is_empty() => return false,
        "tier" if s.tier.is_empty() => return false,
        "specific" | "tier" | "pool" => {}
        _ => return false,
    }
    if (!s.model.is_empty() && !valid_id(&s.model)) || (!s.tier.is_empty() && !valid_id(&s.tier)) {
        return false;
    }
    if s.offerings.is_empty() || s.regions.is_empty() {
        return false;
    }
    if let Some(d) = &s.downgrade_policy {
        if s.mode != "tier" || !valid_id(&d.tier) || d.tier == s.tier || d.offerings.is_empty() {
            return false;
        }
    }
    let mut seen = BTreeSet::new();
    let mut assets = BTreeSet::new();
    for o in s.all_offerings() {
        if !valid_offering(o) || !seen.insert(o.id.as_str()) {
            return false;
        }
        assets.insert(o.price.asset.as_str());
    }
    if assets.len() != 1 {
        return false;
    }
    if !s.regions.iter().all(|r| valid_id(r)) || !s.required_labels.iter().all(|l| valid_id(l)) {
        return false;
    }
    if !valid_price(&s.price) || s.platform_price.as_ref().is_some_and(|p| !valid_price(p)) {
        return false;
    }
    if !(1..=5).contains(&s.max_attempts) || !(1..=MAX_OUTPUT_TOKENS).contains(&s.max_output_tokens) {
        return false;
    }
    if !valid_atoms(&s.max_provider_cost) || !valid_atoms(&s.cost_scale) || s.cost_scale.bytes().all(|b| b == b'0') {
        return false;
    }
    let w = s.weights;
    if ![w.cost, w.latency, w.reliability, w.quality].into_iter().all(|f| f >= 0.0 && f.is_finite()) {
        return false;
    }
    if (w.cost + w.latency + w.reliability + w.quality - 1.0).abs() > 1e-9 {
        return false;
    }
    s.latency_scale_ms > 0.0 && s.latency_scale_ms.is_finite() && (0.0..=1.0).contains(&s.unknown_score)
}

/// Validates a service definition exactly as the TypeScript reference does: unknown fields, out-of-range
/// numbers, malformed ids or amounts, weights that do not sum to one, duplicate offerings and mixed supply
/// currencies are all rejected with `invalid_request`.
pub fn parse_service(data: &[u8]) -> Result<Service, RouterError> {
    let tree = decode_tree(data).ok_or_else(invalid_request)?;
    let service: Service = serde_json::from_value(tree).map_err(|_| invalid_request())?;
    if valid_service(&service) {
        Ok(service)
    } else {
        Err(invalid_request())
    }
}

fn valid_content(s: &str) -> bool {
    in_range(s, 1, 32_000)
}

fn valid_schema_name(s: &str) -> bool {
    (1..=64).contains(&s.len()) && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}

/// Whether a value is in `lo..=hi` (a NaN is in no range; JSON cannot carry one, but a caller may build the type).
fn within(x: f64, lo: f64, hi: f64) -> bool {
    x >= lo && x <= hi
}

fn valid_sampling(s: &Sampling) -> bool {
    let any = s.temperature.is_some() || s.top_p.is_some() || s.stop.is_some() || s.seed.is_some();
    any && s.temperature.is_none_or(|t| within(t, 0.0, 2.0))
        && s.top_p.is_none_or(|p| p > 0.0 && p <= 1.0)
        && s.stop.as_ref().is_none_or(|stop| (1..=4).contains(&stop.len()) && stop.iter().all(|x| in_range(x, 1, 64)))
        && s.seed.is_none_or(|n| n.abs() <= MAX_SAFE)
}

fn valid_tags(tags: &std::collections::BTreeMap<String, String>) -> bool {
    let key_ok =
        |k: &str| (1..=64).contains(&k.len()) && k.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b':' | b'-'));
    let value_ok = |v: &str| in_range(v, 1, 256) && !v.chars().any(|c| c < ' ' || c == '\u{7f}');
    (1..=16).contains(&tags.len()) && tags.iter().all(|(k, v)| key_ok(k) && value_ok(v))
}

fn valid_invocation(i: &Invocation) -> bool {
    if i.sampling.as_ref().is_some_and(|s| !valid_sampling(s))
        || i.reasoning_effort.as_deref().is_some_and(|e| !matches!(e, "low" | "medium" | "high"))
        || i.tags.as_ref().is_some_and(|t| !valid_tags(t))
    {
        return false;
    }
    if !valid_id(&i.service) || !valid_id(&i.customer_id) || i.payer_mode != "developer" {
        return false;
    }
    if i.service_version.is_some_and(|v| !(1..=MAX_SAFE).contains(&v)) {
        return false;
    }
    if i.input.messages.len() > 100 {
        return false;
    }
    // Each message's own shape is checked as it is decoded (`Message`'s `Deserialize`).
    if !crate::tools::valid_tool_use(i) {
        return false;
    }
    if let Some(t) = &i.input.task {
        let meter_ok = t.meter.is_empty() || valid_unit_name(&t.meter) || is_media(&t.meter);
        if kind_meter(&t.kind).is_none() || !valid_content(&t.prompt) || !meter_ok {
            return false;
        }
    }
    if let Some(f) = &i.response_format {
        let js = &f.json_schema;
        let strict_ok = js.strict != Some(false);
        let description_ok = js.description.as_deref().is_none_or(|d| length(d) <= 1024);
        if f.kind != "json_schema" || !valid_schema_name(&js.name) || !strict_ok || !description_ok || i.input.task.is_some() {
            return false;
        }
    }
    let l = &i.limits;
    if !(1..=MAX_OUTPUT_TOKENS).contains(&l.max_output_tokens) || !(100..=MAX_TIMEOUT_MS).contains(&l.timeout_ms) {
        return false;
    }
    if l.max_usage.as_ref().is_some_and(|m| !valid_meter_limits(m)) {
        return false;
    }
    l.max_charge.as_ref().is_none_or(|c| valid_id(&c.asset) && valid_atoms(&c.atoms) && c.scale == 9)
}

/// Validates an invocation exactly as the TypeScript reference does and fills its defaults (`payer_mode`
/// "developer", `max_output_tokens` 1, `timeout_ms` 30000, `stream` false, no messages).
pub fn parse_invocation(data: &[u8]) -> Result<Invocation, RouterError> {
    let tree = decode_tree(data).ok_or_else(invalid_request)?;
    let invocation: Invocation = serde_json::from_value(tree).map_err(|_| invalid_request())?;
    if valid_invocation(&invocation) {
        Ok(invocation)
    } else {
        Err(invalid_request())
    }
}

/// Renders a value as JSON with object keys sorted by code point (bytewise) and no whitespace. It is the input of the
/// idempotency digest, so two equal invocations produce the same bytes in every language. Numbers print as
/// JavaScript prints them.
pub fn canonical<T: Serialize>(v: &T) -> Result<String, serde_json::Error> {
    let tree = serde_json::to_value(v)?;
    let mut out = String::new();
    write_canonical(&mut out, &tree);
    Ok(out)
}

fn write_canonical(out: &mut String, v: &Value) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&js_number(n.as_f64().unwrap_or(0.0))),
        Value::String(s) => write_string(out, s),
        Value::Array(items) => {
            out.push('[');
            for (i, e) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(out, e);
            }
            out.push(']');
        }
        Value::Object(m) => {
            // serde_json keeps objects in a BTreeMap, which is already sorted bytewise.
            out.push('{');
            for (i, (k, e)) in m.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_string(out, k);
                out.push(':');
                write_canonical(out, e);
            }
            out.push('}');
        }
    }
}

/// Escapes like `JSON.stringify`: quotes, backslashes and control characters only.
pub(crate) fn write_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// `Number.prototype.toString` for a finite double: shortest round-trip digits, positional notation for
/// exponents from -7 up to 21, scientific outside.
pub fn js_number(x: f64) -> String {
    if x == 0.0 {
        return "0".to_string();
    }
    if !x.is_finite() {
        return "null".to_string();
    }
    let sci = format!("{:e}", x.abs());
    let (mantissa, exponent) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let k = digits.len() as i32;
    let n = exponent + 1;
    let mut out = String::new();
    if x < 0.0 {
        out.push('-');
    }
    if k <= n && n <= 21 {
        out.push_str(&digits);
        out.extend(std::iter::repeat_n('0', (n - k) as usize));
    } else if 0 < n && n <= 21 {
        out.push_str(&digits[..n as usize]);
        out.push('.');
        out.push_str(&digits[n as usize..]);
    } else if -6 < n && n <= 0 {
        out.push_str("0.");
        out.extend(std::iter::repeat_n('0', (-n) as usize));
        out.push_str(&digits);
    } else {
        let e = n - 1;
        out.push_str(&digits[..1]);
        if k > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        let _ = write!(out, "e{}{}", if e < 0 { '-' } else { '+' }, e.abs());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_print_like_javascript() {
        for (x, want) in [
            (1.0, "1"),
            (-0.0, "0"),
            (0.5, "0.5"),
            (123.456, "123.456"),
            (1e21, "1e+21"),
            (1.5e300, "1.5e+300"),
            (1e-7, "1e-7"),
            (0.000001, "0.000001"),
            (123_456_789_012_345_680_000.0, "123456789012345680000"),
            (9_007_199_254_740_991.0, "9007199254740991"),
            (-2.5e-10, "-2.5e-10"),
        ] {
            assert_eq!(js_number(x), want);
        }
    }

    #[test]
    fn garbage_is_rejected_not_panicked_on() {
        for raw in ["", "null", "[]", "\"x\"", "{", "{} {}", "{\"a\":1}"] {
            assert!(parse_invocation(raw.as_bytes()).is_err(), "invocation {raw:?}");
            assert!(parse_service(raw.as_bytes()).is_err(), "service {raw:?}");
        }
    }

    fn invocation(extra: &str) -> Vec<u8> {
        format!(r#"{{"service":"chat","customer_id":"c1","input":{{"messages":[{{"role":"user","content":"hi"}}]}},"limits":{{}}{extra}}}"#)
            .into_bytes()
    }

    #[test]
    fn defaults_are_filled() {
        let i = parse_invocation(&invocation("")).unwrap();
        assert_eq!((i.payer_mode.as_str(), i.limits.max_output_tokens, i.limits.timeout_ms, i.stream), ("developer", 1, 30_000, false));
    }

    #[test]
    fn null_is_refused_even_where_a_field_is_optional() {
        // serde would read these as "absent"; the reference rejects them.
        for extra in [r#","service_version":null"#, r#","response_format":null"#, r#","stream":null"#] {
            assert!(parse_invocation(&invocation(extra)).is_err(), "{extra}");
        }
        let nested =
            br#"{"service":"chat","customer_id":"c1","input":{"messages":[{"role":"user","content":"hi"}],"task":null},"limits":{}}"#;
        assert!(parse_invocation(nested).is_err());
    }

    #[test]
    fn a_json_schema_may_hold_null() {
        let extra = r#","response_format":{"type":"json_schema","json_schema":{"name":"x","schema":{"default":null,"type":"object"}}}"#;
        let i = parse_invocation(&invocation(extra)).unwrap();
        assert!(canonical(&i).unwrap().contains(r#""schema":{"default":null,"type":"object"}"#));
    }

    #[test]
    fn duplicate_keys_keep_the_last_and_trailing_data_is_refused() {
        let dup =
            br#"{"service":"x","service":"chat","customer_id":"c1","input":{"messages":[{"role":"user","content":"hi"}]},"limits":{}}"#;
        assert_eq!(parse_invocation(dup).unwrap().service, "chat");
        let mut trailing = invocation("");
        trailing.extend_from_slice(b" {}");
        assert!(parse_invocation(&trailing).is_err());
    }

    #[test]
    fn json_keys_are_case_sensitive_and_unknown_keys_refused() {
        assert!(parse_invocation(&invocation(r#","Stream":true"#)).is_err());
        assert!(parse_invocation(&invocation(r#","surprise":1"#)).is_err());
    }

    #[test]
    fn canonical_sorts_keys_and_escapes_like_json_stringify() {
        let v: Value = serde_json::from_str(r#"{"b":[1,2.0,"é\n\u0001"],"a":{"z":true,"y":null}}"#).unwrap();
        assert_eq!(canonical(&v).unwrap(), "{\"a\":{\"y\":null,\"z\":true},\"b\":[1,2,\"é\\n\\u0001\"]}");
    }
}
