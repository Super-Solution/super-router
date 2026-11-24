//! Which offerings to leave alone for a while.
//!
//! When an offering's own failure repeats (its key is rejected, its account is empty, its model is gone, it is rate
//! limited), every request that reaches it first pays one wasted attempt before the router falls through. A
//! [`HealthBook`] remembers the failure and lets routing skip that offering until it is worth trying again.
//!
//! It is a pure state machine: no clock, no thread, no I/O. Every call is given `now_ms`, so a test replays a whole
//! incident in microseconds and the Rust and Go copies are checked against the same sequences.
//!
//! The rules, each of which a conformance case pins:
//! - Only an offering-scoped [`FailureClass`] is recorded. A refused request says nothing about the offering, and a
//!   server error leaves the outcome unknown, so neither cools anything.
//! - A failure starts a cooldown whose base length depends on the class (a rate limit clears in seconds, a rejected
//!   key only when someone fixes it) and doubles with each consecutive failure, up to a ceiling.
//! - A failure reported while the offering is already cooling is ignored: it belongs to a request that was in flight
//!   before the cooldown began, and counting it would stretch the cooldown for one incident.
//! - When a cooldown ends the offering is not simply reopened. The first caller is admitted as a **probe**; everyone
//!   else keeps skipping it until the probe reports (or its lease runs out). One success clears the record; one
//!   failure starts the next, longer, cooldown. A recovering offering is therefore tried by one request, not by the
//!   whole queue.

use std::collections::HashMap;

use super_router_contracts::FailureClass;

/// How long each class of failure keeps an offering out, and how that grows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthPolicy {
    pub rate_limited_ms: i64,
    pub quota_exhausted_ms: i64,
    pub credentials_rejected_ms: i64,
    pub model_unavailable_ms: i64,
    pub region_blocked_ms: i64,
    /// The longest a cooldown can grow to, however many failures in a row.
    pub max_ms: i64,
    /// How long a probe may stay unanswered before another caller is admitted in its place.
    pub probe_lease_ms: i64,
}

impl Default for HealthPolicy {
    fn default() -> Self {
        Self {
            rate_limited_ms: 5_000,
            quota_exhausted_ms: 300_000,
            credentials_rejected_ms: 300_000,
            model_unavailable_ms: 600_000,
            region_blocked_ms: 600_000,
            max_ms: 1_800_000,
            probe_lease_ms: 30_000,
        }
    }
}

impl HealthPolicy {
    fn base(&self, class: FailureClass) -> i64 {
        match class {
            FailureClass::RateLimited => self.rate_limited_ms,
            FailureClass::QuotaExhausted => self.quota_exhausted_ms,
            FailureClass::CredentialsRejected => self.credentials_rejected_ms,
            FailureClass::ModelUnavailable => self.model_unavailable_ms,
            FailureClass::RegionBlocked => self.region_blocked_ms,
            FailureClass::RequestRejected | FailureClass::Unknown => 0,
        }
    }
}

/// What a caller is told about an offering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admit {
    /// Nothing is known against it.
    Open,
    /// Its cooldown has ended and this caller is the one to try it. Report the outcome.
    Probe,
    /// Skip it until `until_ms` (the end of the cooldown, or of the running probe's lease).
    Cooling { until_ms: i64, class: FailureClass },
}

#[derive(Debug, Clone)]
struct Record {
    class: FailureClass,
    streak: u32,
    until_ms: i64,
    probe_until_ms: Option<i64>,
}

/// The failures remembered per offering. Hosts share one book between requests behind a lock.
#[derive(Debug, Clone, Default)]
pub struct HealthBook {
    policy: HealthPolicy,
    records: HashMap<String, Record>,
}

impl HealthBook {
    pub fn new(policy: HealthPolicy) -> Self {
        Self { policy, records: HashMap::new() }
    }

    /// Whether `key` may be tried now. A [`Probe`](Admit::Probe) answer claims the single probe for its lease.
    pub fn admit(&mut self, key: &str, now_ms: i64) -> Admit {
        let lease = self.policy.probe_lease_ms;
        let Some(r) = self.records.get_mut(key) else { return Admit::Open };
        if now_ms < r.until_ms {
            return Admit::Cooling { until_ms: r.until_ms, class: r.class };
        }
        match r.probe_until_ms {
            Some(until_ms) if now_ms < until_ms => Admit::Cooling { until_ms, class: r.class },
            _ => {
                r.probe_until_ms = Some(now_ms.saturating_add(lease));
                Admit::Probe
            }
        }
    }

    /// Records that an attempt on `key` failed with `class`. Classes that do not belong to the offering are ignored.
    pub fn failed(&mut self, key: &str, class: FailureClass, now_ms: i64) {
        if !class.is_offering_scoped() {
            return;
        }
        let base = self.policy.base(class).max(0);
        let streak = match self.records.get(key) {
            Some(r) if now_ms < r.until_ms => return,
            Some(r) => r.streak.saturating_add(1),
            None => 1,
        };
        // base * 2^(streak-1), saturating, never above the ceiling.
        let grown = base.saturating_mul(1_i64 << streak.saturating_sub(1).min(32));
        let length = grown.min(self.policy.max_ms.max(base));
        self.records.insert(key.to_string(), Record { class, streak, until_ms: now_ms.saturating_add(length), probe_until_ms: None });
    }

    /// Records that an attempt on `key` succeeded: whatever was held against it is cleared.
    pub fn succeeded(&mut self, key: &str) {
        self.records.remove(key);
    }

    /// How many offerings currently have something held against them.
    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
}
