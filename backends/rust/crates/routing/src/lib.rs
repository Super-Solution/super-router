//! Offering selection, ported from `route` in `packages/core` of the TypeScript reference: hard limits first,
//! then weighted scoring, ordered by score and then by id. It reproduces every case in
//! `conformance/routing.v1.json`, including the exact IEEE-754 scores. Offering ids must sort the same
//! bytewise as in TypeScript (use `[a-z0-9]`).

use std::cmp::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};

use num_bigint::BigInt;
use num_traits::Zero;
use serde::{Deserialize, Serialize};
use super_router_contracts::{parse_utc_millis, Invocation, Offering, RouterError, Service};
use super_router_pricing::{atoms, bound, charge, valid_unit_name, Price, Usage};

/// An offering that passed every hard limit.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub offering: Offering,
    pub cost: BigInt,
    pub downgrade: bool,
    pub score: f64,
}

/// An offering that was dropped and the first reason that applied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Excluded {
    pub id: String,
    pub reason: String,
}

/// The most a single attempt can cost on one price: the token bound for chat, or the priced media limits for a
/// task. Routing and the router both use it, so the reserved amount and the candidate cost can never disagree.
pub fn invocation_bound(input: &Invocation, max_input: i64, price: &Price) -> Result<BigInt, RouterError> {
    if input.input.task.is_none() {
        return bound(max_input, input.limits.max_output_tokens, price);
    }
    let mut usage = Usage::default();
    if let Some(limits) = &input.limits.max_usage {
        for (k, n) in limits.iter() {
            let target = if valid_unit_name(k) { &mut usage.units } else { &mut usage.media };
            target.insert(k.clone(), *n);
        }
    }
    charge(&usage, price)
}

fn min_one(x: f64) -> f64 {
    if x < 1.0 {
        x
    } else {
        1.0
    }
}

/// A big integer as the nearest double, the way the reference's `Number(bigint)` converts it.
fn to_f64(n: &BigInt) -> f64 {
    n.to_string().parse().unwrap_or(f64::INFINITY)
}

fn label_reason(s: &Service, o: &Offering, now_ms: i64) -> Option<&'static str> {
    for label in &s.required_labels {
        let expiry = o.label_evidence(label).and_then(|e| e.expires_at.as_deref()).and_then(parse_utc_millis);
        if expiry.is_some_and(|expiry| expiry <= now_ms) {
            return Some("label_expired");
        }
    }
    if s.require_verified_labels == Some(true) {
        for label in &s.required_labels {
            if o.label_evidence(label).is_none_or(|e| e.verification != "verified") {
                return Some("label_unverified");
            }
        }
    }
    None
}

fn exclusion(service: &Service, input: &Invocation, o: &Offering, now_ms: i64) -> Option<&'static str> {
    let task_kind = input.input.task.as_ref().map(|t| t.kind.as_str());
    if o.task_kind.as_deref() != task_kind || service.task_kind.as_deref() != task_kind {
        return Some("task_kind_mismatch");
    }
    if !o.enabled {
        return Some("disabled");
    }
    if service.mode == "specific" && o.model != service.model {
        return Some("model_mismatch");
    }
    if !service.regions.contains(&o.region) {
        return Some("region_mismatch");
    }
    if service.required_labels.iter().any(|r| !o.labels.contains(r)) {
        return Some("label_mismatch");
    }
    if let Some(reason) = label_reason(service, o, now_ms) {
        return Some(reason);
    }
    if input.limits.max_output_tokens > o.max_output_tokens {
        return Some("output_limit");
    }
    if input.response_format.is_some() && !o.has_capability("structured_output") {
        return Some("structured_output_unsupported");
    }
    if let Some(task) = &input.input.task {
        let key = task.meter_name();
        let units = input.limits.usage_limit(&key);
        let priced = o.price.media.contains_key(&key) || o.price.units.contains_key(&key);
        if units == 0 || units > o.max_usage.get(&key) || !priced {
            return Some("meter_limit");
        }
    }
    None
}

/// Applies the hard limits and then scores what is left, using the wall clock for label expiry. A pricing error
/// from a task bound (an unpriced meter) aborts the whole call, as in the reference.
pub fn route(service: &Service, input: &Invocation) -> Result<(Vec<Candidate>, Vec<Excluded>), RouterError> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
    route_at(service, input, now)
}

/// [`route`] with an injected time (milliseconds since the epoch).
pub fn route_at(service: &Service, input: &Invocation, now_ms: i64) -> Result<(Vec<Candidate>, Vec<Excluded>), RouterError> {
    let mut candidates = Vec::new();
    let mut excluded = Vec::new();
    let max_provider_cost = atoms(&service.max_provider_cost)?;
    let cost_scale = atoms(&service.cost_scale)?;
    if cost_scale.is_zero() {
        return Err(RouterError::new("invalid_service", 500));
    }
    for o in service.all_offerings() {
        let mut reason = exclusion(service, input, o, now_ms);
        let mut cost = BigInt::zero();
        if reason.is_none() {
            cost = invocation_bound(input, o.max_input_tokens, &o.price)?;
            if cost > max_provider_cost {
                reason = Some("budget_exceeded");
            }
        }
        if let Some(reason) = reason {
            excluded.push(Excluded { id: o.id.clone(), reason: reason.to_string() });
            continue;
        }
        let ratio = to_f64(&(&cost * 1_000_000 / &cost_scale)) / 1_000_000.0;
        let unknown = service.unknown_score;
        let latency = o.latency_ms.map_or(unknown, |ms| 1.0 - min_one(ms / service.latency_scale_ms));
        let reliability = o.reliability.unwrap_or(unknown);
        let quality = o.quality.unwrap_or(unknown);
        // Rust never fuses a multiply into the next add, so the last bit matches the reference on every target.
        let w = service.weights;
        let mut score = w.cost * (1.0 - min_one(ratio)) + w.latency * latency;
        score += w.reliability * reliability;
        score += w.quality * quality;
        let downgrade =
            service.mode == "tier" && service.downgrade_policy.as_ref().is_some_and(|d| d.offerings.iter().any(|lower| lower.id == o.id));
        candidates.push(Candidate { offering: o.clone(), cost, downgrade, score });
    }
    candidates.sort_by(|a, b| {
        a.downgrade
            .cmp(&b.downgrade)
            .then_with(|| b.score.partial_cmp(&a.score).unwrap_or(Ordering::Equal))
            .then_with(|| a.offering.id.cmp(&b.offering.id))
    });
    Ok((candidates, excluded))
}
