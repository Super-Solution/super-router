//! Exact pricing for Super Router.
//!
//! Mirrors `packages/core` (`charge`, `bound`) and `credit-rates.ts` (`compileCreditPrice`) of the TypeScript
//! reference, as the Go and Python backends do. Money is integer atoms held in [`BigInt`], so results match the
//! reference's `BigInt` exactly. `conformance/pricing.v1.json` is the shared test data.
//!
//! Beyond the reference, [`RateCard`] prices the kinds of token the wire [`Price`] has no field for (one-hour cache
//! writes, reasoning tokens) and a per-call fee, from one value with the usual fallbacks. A [`RateSchedule`] picks the
//! card in effect at an instant, for prices that change on a date.

use std::collections::BTreeMap;
use std::fmt;

use num_bigint::BigInt;
use num_integer::Integer;
use num_traits::{One, Zero};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

mod rate_card;
mod rate_schedule;
pub use rate_card::{EffectiveRates, RateCard, TokenCounts, TokenKind};
pub use rate_schedule::{RateSchedule, RateStep};

/// The fixed accounting scale of one credit.
pub const CREDIT_ATOMS: i64 = 1_000_000_000;

/// The media meters, in the order the reference iterates them.
pub const MEDIA_KEYS: [&str; 4] = ["images", "video_ms", "audio_ms", "tasks"];
const TOKEN_KEYS: [&str; 4] = ["input", "cached", "cache_write", "output"];
const RESERVED_UNIT_NAMES: [&str; 5] = ["units", "__proto__", "constructor", "prototype", ""];

/// A stable, client-visible error code with an HTTP status, like the TypeScript `RouterError`.
#[derive(Debug, Clone, PartialEq)]
pub struct RouterError {
    pub code: String,
    pub status: u16,
    /// The same call can succeed later without changes (a window that resets).
    pub retryable: bool,
    /// Safe, customer-visible facts about the refusal, such as which window and when it resets.
    pub details: Option<Map<String, Value>>,
}

impl RouterError {
    pub fn new(code: impl Into<String>, status: u16) -> Self {
        Self { code: code.into(), status, retryable: false, details: None }
    }
}

impl fmt::Display for RouterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.code)
    }
}

impl std::error::Error for RouterError {}

/// Prices one media meter: `atoms` per `per` units.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaRate {
    pub atoms: String,
    pub per: i64,
}

/// A custom unit rate: an exact rational rate plus the display unit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnitRate {
    pub atoms: String,
    pub per: i64,
    pub display_unit: String,
}

fn one() -> i64 {
    1
}

/// The wire price. Token rates are atoms per `per` tokens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Price {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub units: BTreeMap<String, UnitRate>,
    pub asset: String,
    pub input: String,
    pub cached: String,
    pub cache_write: String,
    pub output: String,
    #[serde(default = "one")]
    pub per: i64,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub media: BTreeMap<String, MediaRate>,
}

/// Meter readings. Media meters and custom units that are absent count as zero.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Usage {
    pub units: BTreeMap<String, i64>,
    pub input: i64,
    pub cached: i64,
    pub cache_write: i64,
    pub output: i64,
    pub media: BTreeMap<String, i64>,
}

fn invalid_price() -> RouterError {
    RouterError::new("invalid_price", 500)
}

/// Parses a decimal atom amount. Prices are validated on the way in; this only guards stored data.
pub fn atoms(s: &str) -> Result<BigInt, RouterError> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid_price());
    }
    s.parse::<BigInt>().map_err(|_| invalid_price())
}

fn ceil_div(numerator: &BigInt, denominator: &BigInt) -> BigInt {
    (numerator + denominator - BigInt::one()) / denominator
}

fn rate_part(units: i64, atoms_text: &str, per: i64) -> Result<BigInt, RouterError> {
    if per < 1 {
        return Err(invalid_price());
    }
    Ok(ceil_div(&(BigInt::from(units) * atoms(atoms_text)?), &BigInt::from(per)))
}

/// The customer charge in atoms for one usage record.
pub fn charge(u: &Usage, p: &Price) -> Result<BigInt, RouterError> {
    if p.per < 1 {
        return Err(invalid_price());
    }
    let mut numerator = BigInt::zero();
    for (units, rate) in [(u.input, &p.input), (u.cached, &p.cached), (u.cache_write, &p.cache_write), (u.output, &p.output)] {
        numerator += BigInt::from(units) * atoms(rate)?;
    }
    let mut total = ceil_div(&numerator, &BigInt::from(p.per));
    for key in MEDIA_KEYS {
        let units = u.media.get(key).copied().unwrap_or(0);
        match p.media.get(key) {
            None if units != 0 => return Err(RouterError::new("unpriced_meter", 422)),
            None => {}
            Some(rate) => total += rate_part(units, &rate.atoms, rate.per)?,
        }
    }
    for (key, &units) in &u.units {
        match p.units.get(key) {
            None if units != 0 => return Err(RouterError::new("unpriced_meter", 422)),
            None => {}
            Some(rate) => total += rate_part(units, &rate.atoms, rate.per)?,
        }
    }
    Ok(total)
}

/// The upper bound reserved before a call: the dearest input rate times `input`, plus `output`.
pub fn bound(input: i64, output: i64, p: &Price) -> Result<BigInt, RouterError> {
    if p.per < 1 {
        return Err(invalid_price());
    }
    let mut rate = atoms(&p.input)?;
    for other in [&p.cached, &p.cache_write] {
        let other = atoms(other)?;
        if other > rate {
            rate = other;
        }
    }
    let sum = BigInt::from(input) * rate + BigInt::from(output) * atoms(&p.output)?;
    Ok(ceil_div(&sum, &BigInt::from(p.per)))
}

/// Rejects token names, aliases and unsafe object keys: `^[a-z][a-z0-9_]{0,63}$` minus the reserved names.
pub fn valid_unit_name(key: &str) -> bool {
    let b = key.as_bytes();
    if b.is_empty() || b.len() > 64 || !b[0].is_ascii_lowercase() {
        return false;
    }
    if !b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_') {
        return false;
    }
    !RESERVED_UNIT_NAMES.contains(&key) && !TOKEN_KEYS.contains(&key) && !MEDIA_KEYS.contains(&key)
}

/// `^[a-zA-Z0-9_.:/-]+$`, at most 120 characters.
pub fn valid_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= 120 && s.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b':' | b'/' | b'-'))
}

/// `^(0|[1-9][0-9]{0,29})$`
pub fn valid_atoms(s: &str) -> bool {
    match s.as_bytes() {
        [b'0'] => true,
        [first, rest @ ..] => (b'1'..=b'9').contains(first) && rest.len() <= 29 && rest.iter().all(u8::is_ascii_digit),
        [] => false,
    }
}

/// `^[1-9][0-9]{0,29}$`
fn valid_integer(s: &str) -> bool {
    valid_atoms(s) && s != "0"
}

fn invalid_rates() -> RouterError {
    RouterError::new("invalid_credit_rates", 400)
}

struct Ratio {
    units: BigInt,
    credits: BigInt,
}

fn parse_ratio(v: &Value) -> Result<Ratio, RouterError> {
    let m = v.as_object().ok_or_else(invalid_rates)?;
    if m.keys().any(|k| k != "units" && k != "credits") {
        return Err(invalid_rates());
    }
    let units = m.get("units").ok_or_else(invalid_rates)?;
    let one = Value::String("1".into());
    let credits = m.get("credits").unwrap_or(&one);
    let mut out = [BigInt::zero(), BigInt::zero()];
    for (slot, item) in out.iter_mut().zip([units, credits]) {
        let s = item.as_str().filter(|s| valid_integer(s)).ok_or_else(invalid_rates)?;
        *slot = atoms(s)?;
    }
    let [units, credits] = out;
    Ok(Ratio { units, credits })
}

fn reduce(r: &Ratio) -> (BigInt, BigInt) {
    let numerator = BigInt::from(CREDIT_ATOMS) * &r.credits;
    let denominator = r.units.clone();
    let g = numerator.gcd(&denominator);
    (numerator / &g, denominator / g)
}

fn keys_within(m: &Map<String, Value>, allowed: &[&str]) -> bool {
    m.keys().all(|k| allowed.contains(&k.as_str()))
}

fn precision_error() -> RouterError {
    RouterError::new("unsupported_rate_precision", 422)
}

fn to_per(n: &BigInt) -> Result<i64, RouterError> {
    i64::try_from(n).map_err(|_| precision_error())
}

/// Compiles exact rational units-per-credit rates into the wire [`Price`]. `raw` is a decoded JSON value.
pub fn compile_credit_price(raw: &Value) -> Result<Price, RouterError> {
    let cfg = raw.as_object().filter(|c| keys_within(c, &["asset", "tokens", "media", "units"])).ok_or_else(invalid_rates)?;
    let asset = cfg.get("asset").and_then(Value::as_str).filter(|a| valid_id(a)).ok_or_else(invalid_rates)?;

    let mut tokens: Option<BTreeMap<&str, Ratio>> = None;
    if let Some(v) = cfg.get("tokens") {
        let m = v.as_object().filter(|m| m.len() == TOKEN_KEYS.len() && keys_within(m, &TOKEN_KEYS)).ok_or_else(invalid_rates)?;
        let mut parsed = BTreeMap::new();
        for k in TOKEN_KEYS {
            parsed.insert(k, parse_ratio(&m[k])?);
        }
        tokens = Some(parsed);
    }
    let mut media: BTreeMap<&str, Ratio> = BTreeMap::new();
    if let Some(v) = cfg.get("media") {
        let m = v.as_object().filter(|m| keys_within(m, &MEDIA_KEYS)).ok_or_else(invalid_rates)?;
        for (k, item) in m {
            media.insert(k.as_str(), parse_ratio(item)?);
        }
    }
    let has_units = cfg.get("units").is_some_and(|u| !u.is_null());
    if tokens.is_none() && media.is_empty() && !has_units {
        return Err(invalid_rates());
    }

    let limit = BigInt::from(1_000_000_000_i64);
    let mut numerators = Vec::with_capacity(TOKEN_KEYS.len());
    let mut denominators = Vec::with_capacity(TOKEN_KEYS.len());
    let mut per = BigInt::one();
    for k in TOKEN_KEYS {
        let (n, d) = match &tokens {
            Some(t) => reduce(&t[k]),
            None => (BigInt::zero(), BigInt::one()),
        };
        let g = per.gcd(&d);
        per = (&per / g) * &d;
        if per > limit {
            return Err(precision_error());
        }
        numerators.push(n);
        denominators.push(d);
    }
    let values: Vec<String> = numerators.iter().zip(&denominators).map(|(n, d)| (n * (&per / d)).to_string()).collect();
    let mut out = Price {
        units: BTreeMap::new(),
        asset: asset.to_string(),
        input: values[0].clone(),
        cached: values[1].clone(),
        cache_write: values[2].clone(),
        output: values[3].clone(),
        per: to_per(&per)?,
        media: BTreeMap::new(),
    };
    for (k, ratio) in &media {
        let (numerator, denominator) = reduce(ratio);
        if denominator > limit {
            return Err(precision_error());
        }
        out.media.insert((*k).to_string(), MediaRate { atoms: numerator.to_string(), per: to_per(&denominator)? });
    }
    if let Some(v) = cfg.get("units") {
        let m = v.as_object().ok_or_else(invalid_rates)?;
        for (key, item) in m {
            let r = item
                .as_object()
                .filter(|r| keys_within(r, &["units", "credits", "display_unit"]) && valid_unit_name(key))
                .ok_or_else(invalid_rates)?;
            let display = r.get("display_unit").and_then(Value::as_str).ok_or_else(invalid_rates)?;
            let n = display.chars().count();
            if !(1..=80).contains(&n) {
                return Err(invalid_rates());
            }
            let mut numeric = Map::new();
            if let Some(u) = r.get("units") {
                numeric.insert("units".into(), u.clone());
            }
            if let Some(c) = r.get("credits") {
                numeric.insert("credits".into(), c.clone());
            }
            let (numerator, denominator) = reduce(&parse_ratio(&Value::Object(numeric))?);
            if denominator > limit || !valid_atoms(&numerator.to_string()) {
                return Err(precision_error());
            }
            out.units.insert(
                key.clone(),
                UnitRate { atoms: numerator.to_string(), per: to_per(&denominator)?, display_unit: display.to_string() },
            );
        }
    }
    // The wire price caps atoms at 30 digits.
    let oversize = values.iter().chain(out.media.values().map(|r| &r.atoms)).any(|a| !valid_atoms(a));
    if oversize {
        return Err(precision_error());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_names() {
        assert!(valid_unit_name("tool_calls"));
        for bad in ["", "Units", "units", "input", "images", "__proto__", "1a", "a-b", "constructor"] {
            assert!(!valid_unit_name(bad), "{bad}");
        }
        assert!(valid_unit_name(&"a".repeat(64)));
        assert!(!valid_unit_name(&"a".repeat(65)));
    }

    #[test]
    fn atom_text() {
        assert!(valid_atoms("0") && valid_atoms("10") && valid_atoms(&"9".repeat(30)));
        assert!(!valid_atoms("") && !valid_atoms("01") && !valid_atoms("-1") && !valid_atoms(&"9".repeat(31)));
    }
}
