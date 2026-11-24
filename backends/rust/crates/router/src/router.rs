//! The data plane: admit an invocation idempotently, pick offerings, reserve the customer's funds, run the
//! provider and settle or release the reservation.
//!
//! This is `Router.infer` of the TypeScript reference. Storage is the [`Store`] port, providers are the
//! [`Provider`] trait, and nothing here touches HTTP. An uncertain provider outcome is never refunded: the
//! reservation stays pending for reconciliation.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt;
use hmac::{Hmac, Mac};
use num_bigint::BigInt;
use serde::Serialize;
use serde_json::{json, Value};
use sha2::Sha256;
use super_router_contracts::{
    canonical, invocation_needs, js_trim, parse_invocation, valid_signature, FailureClass, Invocation, Meters, Offering, Principal,
    Provider, ProviderError, ProviderEvent, RouterError, Service, StreamEvent, ToolCall, ToolCalls,
};
use super_router_pricing::{atoms, charge};
use super_router_routing::{invocation_bound, route_at, Admit, Candidate, Excluded, HealthBook};
use tokio::time::{interval_at, Instant, MissedTickBehavior};
use tokio_util::sync::CancellationToken;

use crate::store::{system_clock, Clock, Decision, DecisionCandidate, Leg, RequestRecord, Store, StoreError};

/// How often a running request checks whether somebody asked to cancel it.
const CANCEL_POLL: Duration = Duration::from_millis(100);
/// The most output one request may accumulate: text and tool-call arguments together.
const MAX_OUTPUT_BYTES: usize = 1_000_000;
/// The most tool calls one answer may hold.
const MAX_TOOL_CALLS: usize = 64;
/// The largest index a JavaScript client can read exactly.
const MAX_SAFE_INDEX: usize = (1 << 53) - 1;

/// The assistant's answer. `tool_calls` holds the calls of a finished answer only: a call cut off by a failure
/// is never reported as callable.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Output {
    pub role: String,
    pub content: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
}

/// Tells the caller whether usage is known: `confirmed`, `pending` or `not_billable`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UsageReport {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meters: Option<Meters>,
}

/// What one account is charged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Charge {
    pub kind: String,
    pub asset: String,
    pub atoms: String,
    pub scale: u32,
}

/// The charges of a settled request, empty otherwise.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BillingReport {
    pub charges: Vec<Charge>,
}

/// An actual cross-tier attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct Downgrade {
    pub from_tier: String,
    pub to_tier: String,
}

/// The answer to [`Router::infer`]. A replayed idempotency key returns only the identifiers and statuses.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InferResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub downgrade: Option<Downgrade>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub replayed: bool,
    pub id: String,
    pub execution_status: String,
    pub billing_status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_version: Option<i64>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub model: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub tier: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<Output>,
    /// `stop`, `length` or `tool_calls`; absent when the request did not finish.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<UsageReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub billing: Option<BillingReport>,
}

/// A failed inference. Errors before funds are reserved carry no request id; later ones do.
#[derive(Debug, Clone, PartialEq)]
pub struct InferError {
    pub error: RouterError,
    pub request_id: Option<String>,
}

impl std::fmt::Display for InferError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.error.code)
    }
}

impl std::error::Error for InferError {}

impl From<StoreError> for InferError {
    /// Before a reservation exists, an internal failure is just an internal error.
    fn from(e: StoreError) -> Self {
        let error = match e {
            StoreError::Router(re) => re,
            StoreError::Internal(_) => RouterError::new("internal_error", 500),
        };
        Self { error, request_id: None }
    }
}

impl From<RouterError> for InferError {
    fn from(error: RouterError) -> Self {
        Self { error, request_id: None }
    }
}

/// The caller stopped listening, so the provider must stop and the outcome is recorded as uncertain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmitClosed;

/// Receives the stream events of a streaming inference, in order.
pub type Emit = Box<dyn FnMut(StreamEvent) -> Result<(), EmitClosed> + Send>;

/// Runs invocations against a [`Store`] and a set of providers. It is safe for concurrent use.
pub struct Router {
    store: Arc<dyn Store>,
    providers: HashMap<String, Arc<dyn Provider>>,
    digest_key: Vec<u8>,
    clock: Clock,
    health: Option<Arc<Mutex<HealthBook>>>,
}

fn fail(code: &str, status: u16) -> RouterError {
    RouterError::new(code, status)
}

/// The identifiers and statuses of an already admitted request.
fn replayed(rec: &RequestRecord) -> InferResult {
    InferResult {
        downgrade: None,
        replayed: true,
        id: rec.id.clone(),
        execution_status: rec.execution.clone(),
        billing_status: rec.billing.clone(),
        service_version: None,
        model: String::new(),
        tier: String::new(),
        output: None,
        finish_reason: None,
        usage: None,
        billing: None,
    }
}

/// Applies the cross-field rules of an invocation: chat has messages and no media limits, a task has no
/// messages and exactly its own meter limited.
fn validate_shape(input: &Invocation) -> Result<(), RouterError> {
    if let Some(task) = &input.input.task {
        let meter = task.meter_name();
        let limits = input.limits.max_usage.as_ref();
        let limit = |k: &str| limits.map_or(0, |m| m.get(k));
        let others = limits.is_some_and(|m| m.iter().any(|(k, n)| *k != meter && *n != 0));
        let tasks_ok = meter != "tasks" || limit("tasks") == 1;
        if !input.input.messages.is_empty() || limit(&meter) == 0 || others || !tasks_ok {
            return Err(fail("invalid_task_limits", 400));
        }
        return Ok(());
    }
    if input.input.messages.is_empty() || input.limits.max_usage.is_some() {
        return Err(fail("invalid_request", 400));
    }
    Ok(())
}

/// Applies the per-kind limits to what a provider reported. Returns an error code, or `None`.
fn check_usage(input: &Invocation, u: &Meters, completion: &str) -> Option<&'static str> {
    if let Some(task) = &input.input.task {
        if [u.input, u.cached, u.cache_write, u.output].iter().any(|n| *n != 0) {
            return Some("usage_out_of_bounds");
        }
        if u.quantities().iter().any(|(k, n)| *n > input.limits.usage_limit(k)) {
            return Some("usage_out_of_bounds");
        }
        if task.meter_name() == "tasks" && u.media("tasks") != i64::from(completion == "succeeded") {
            return Some("usage_out_of_bounds");
        }
        return None;
    }
    u.quantities().values().any(|n| *n != 0).then_some("unexpected_meter")
}

struct Admitted {
    input: Invocation,
    service: Service,
    candidates: Vec<Candidate>,
    legs: Vec<Leg>,
    request: RequestRecord,
}

enum Admission {
    Replayed(Box<RequestRecord>),
    Fresh(Box<Admitted>),
}

/// Mutable facts about one running request that the failure path also needs.
struct Run {
    started: bool,
    seq: u32,
    emit: Option<Emit>,
}

impl Run {
    fn send(&mut self, request_id: &str, kind: &str, data: Value) -> Result<(), StoreError> {
        self.seq += 1;
        match self.emit.as_mut() {
            None => Ok(()),
            Some(emit) => emit(StreamEvent { kind: kind.to_string(), request_id: request_id.to_string(), sequence: self.seq, data })
                .map_err(|EmitClosed| StoreError::Internal("the client stopped listening".into())),
        }
    }
}

/// The key a health book remembers an offering under: offering ids are only unique within a provider.
fn health_key(o: &Offering) -> String {
    format!("{}/{}", o.provider, o.id)
}

impl Router {
    /// Drops the candidates the health book says to leave alone, recording each as excluded with the reason
    /// `cooling_down`. If that would leave nothing, every candidate is kept: a request that might fail is better
    /// than one that certainly does, and the attempts will refresh the book.
    fn without_cooling(&self, candidates: Vec<Candidate>, excluded: &mut Vec<Excluded>) -> Vec<Candidate> {
        let Some(health) = &self.health else { return candidates };
        let now = (self.clock)();
        let mut book = health.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let answers: Vec<Admit> = candidates.iter().map(|c| book.admit(&health_key(&c.offering), now)).collect();
        if answers.iter().all(|a| matches!(a, Admit::Cooling { .. })) {
            return candidates;
        }
        let mut kept = Vec::with_capacity(candidates.len());
        for (c, a) in candidates.into_iter().zip(answers) {
            if matches!(a, Admit::Cooling { .. }) {
                excluded.push(Excluded { id: c.offering.id.clone(), reason: "cooling_down".into() });
            } else {
                kept.push(c);
            }
        }
        kept
    }

    /// Tells the health book how an attempt ended. Only a clean success and a failure that belongs to the offering
    /// count; everything else (a refused request, an unknown outcome) says nothing about it.
    fn note_health(&self, offering: &Offering, outcome: Result<(), &ProviderError>) {
        let Some(health) = &self.health else { return };
        let now = (self.clock)();
        let mut book = health.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let key = health_key(offering);
        match outcome {
            Ok(()) => book.succeeded(&key),
            Err(e) => {
                if let Some(class) = FailureClass::from_name(&e.code) {
                    book.failed_after(&key, class, now, e.retry_after_ms);
                }
            }
        }
    }

    /// Creates a router. `digest_key` keys the idempotency digest and must be at least 32 bytes.
    pub fn new(store: Arc<dyn Store>, providers: HashMap<String, Arc<dyn Provider>>, digest_key: &str) -> Result<Self, &'static str> {
        if digest_key.len() < 32 {
            return Err("router: digest key must be at least 32 characters");
        }
        Ok(Self { store, providers, digest_key: digest_key.as_bytes().to_vec(), clock: system_clock(), health: None })
    }

    /// Replaces the wall clock, for tests.
    #[must_use]
    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    /// Lets the router skip offerings whose own failure is repeating (a rejected key, an empty account, a retired
    /// model, a rate limit) until the book says they are worth trying again, instead of paying one failed attempt per
    /// request. Without a book nothing is skipped. The book is shared, so hosts can give one to several routers.
    #[must_use]
    pub fn with_health(mut self, health: Arc<Mutex<HealthBook>>) -> Self {
        self.health = Some(health);
        self
    }

    /// The router's storage, for hosts that expose balances or cancellation next to inference.
    pub fn store(&self) -> &Arc<dyn Store> {
        &self.store
    }

    fn digest(&self, input: &Invocation) -> Result<String, StoreError> {
        let canon = canonical(input).map_err(|e| StoreError::Internal(e.to_string()))?;
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.digest_key).map_err(|e| StoreError::Internal(e.to_string()))?;
        mac.update(canon.as_bytes());
        Ok(hex::encode(mac.finalize().into_bytes()))
    }

    /// Runs one invocation. `raw` is the JSON request body and `key` the idempotency key. `emit`, when set,
    /// receives the stream events in order; if it fails the provider is stopped and the outcome is recorded as
    /// uncertain. Cancelling `cancel` aborts the provider call but never the settlement.
    ///
    /// Errors before funds are reserved carry no request id. Later errors carry the id of the request.
    pub async fn infer(
        &self,
        p: &Principal,
        raw: &[u8],
        key: &str,
        emit: Option<Emit>,
        cancel: CancellationToken,
    ) -> Result<InferResult, InferError> {
        match self.admit(p, raw, key).await? {
            Admission::Replayed(rec) => Ok(replayed(&rec)),
            Admission::Fresh(admitted) => self.execute(p, *admitted, emit, cancel).await,
        }
    }

    async fn admit(&self, p: &Principal, raw: &[u8], key: &str) -> Result<Admission, StoreError> {
        if !p.has_scope("invoke") {
            return Err(fail("scope_denied", 403).into());
        }
        let input = parse_invocation(raw)?;
        validate_shape(&input)?;
        if !p.may_use(&input.service) || input.customer_id != p.customer {
            return Err(fail("scope_denied", 403).into());
        }
        if key.is_empty() || key.len() > 200 {
            return Err(fail("missing_idempotency_key", 400).into());
        }
        let digest = self.digest(&input)?;
        if let Some(existing) = self.store.replay(p, key, &digest).await? {
            return Ok(Admission::Replayed(Box::new(existing)));
        }
        let service = self.store.get_service(p, &input.service, input.service_version).await?;
        if input.service_version.is_some() {
            let active = self.store.get_service(p, &input.service, None).await?;
            if active.version != service.version {
                return Err(fail("inactive_service_version", 409).into());
            }
        }
        if input.limits.max_output_tokens > service.max_output_tokens {
            return Err(fail("unsupported_capability", 422).into());
        }
        let (candidates, mut excluded) = route_at(&service, &input, (self.clock)())?;
        if candidates.is_empty() {
            return Err(fail("no_eligible_offering", 422).into());
        }
        let mut providers = Vec::with_capacity(candidates.len());
        for c in &candidates {
            providers.push(self.providers.get(&c.offering.provider).ok_or_else(|| fail("provider_unavailable", 503))?.capabilities());
        }
        // Refused here, before anything is reserved: a provider that cannot carry tools or images would drop them.
        let needs = invocation_needs(&input);
        let mut capable = Vec::with_capacity(candidates.len());
        for (c, caps) in candidates.into_iter().zip(providers) {
            if caps.can(needs) {
                capable.push(c);
            } else {
                excluded.push(Excluded { id: c.offering.id.clone(), reason: "capability_unsupported".into() });
            }
        }
        let candidates = self.without_cooling(capable, &mut excluded);
        if candidates.is_empty() {
            return Err(fail("unsupported_capability", 422).into());
        }
        let mut max_input = 0;
        let mut decision = Decision { excluded, candidates: Vec::with_capacity(candidates.len()) };
        for c in &candidates {
            max_input = max_input.max(c.offering.max_input_tokens);
            let downgrade = c.downgrade.then(|| Downgrade {
                from_tier: service.tier.clone(),
                to_tier: service.downgrade_policy.as_ref().map(|d| d.tier.clone()).unwrap_or_default(),
            });
            decision.candidates.push(DecisionCandidate {
                downgrade,
                quality_metadata: c.offering.quality_metadata.clone(),
                label_evidence: c.offering.label_evidence.clone(),
                id: c.offering.id.clone(),
                score: c.score,
                cost: c.cost.to_string(),
            });
        }
        let mut legs = self.store.accounts(p, &service).await?;
        for leg in &mut legs {
            let bound = invocation_bound(&input, max_input, &leg.price)?;
            leg.reserved = bound.to_string();
            if let (Some(limit), "customer_sale") = (&input.limits.max_charge, leg.kind.as_str()) {
                if limit.asset != leg.price.asset || atoms(&limit.atoms)? < bound {
                    return Err(fail("budget_exceeded", 402).into());
                }
            }
        }
        let deadline = (self.clock)() + input.limits.timeout_ms;
        let reservation = self.store.reserve(p, key, &digest, &service, &legs, deadline, &decision).await?;
        if !reservation.fresh {
            return Ok(Admission::Replayed(Box::new(reservation.request)));
        }
        Ok(Admission::Fresh(Box::new(Admitted { input, service, candidates, legs, request: reservation.request })))
    }

    /// Runs the attempts of an admitted request. From here on store calls are never tied to `cancel`, because a
    /// disconnected client must not leave funds held.
    async fn execute(
        &self,
        p: &Principal,
        admitted: Admitted,
        emit: Option<Emit>,
        cancel: CancellationToken,
    ) -> Result<InferResult, InferError> {
        let request_id = admitted.request.id.clone();
        let mut run = Run { started: false, seq: 0, emit };
        match self.attempts(p, &admitted, &mut run, &cancel).await {
            Ok(result) => Ok(result),
            Err(failure) => {
                // An unexpected storage or transport failure must never turn an uncertain dispatch into a refund.
                let outcome = if run.started { "partial" } else { "unknown" };
                let _ = self.store.finish(p, &request_id, outcome, None, true).await;
                let error = match failure {
                    StoreError::Router(re) => re,
                    StoreError::Internal(_) => fail("execution_unknown", 503),
                };
                Err(InferError { error, request_id: Some(request_id) })
            }
        }
    }

    async fn attempts(&self, p: &Principal, a: &Admitted, run: &mut Run, cancel: &CancellationToken) -> Result<InferResult, StoreError> {
        let Admitted { input, service, candidates, legs, request } = a;
        let id = request.id.as_str();
        let timeout = tokio::time::sleep(Duration::from_millis(u64::try_from(input.limits.timeout_ms).unwrap_or(0)));
        tokio::pin!(timeout);
        let mut poll = interval_at(Instant::now() + CANCEL_POLL, CANCEL_POLL);
        poll.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut aborted = false;

        let mut text = String::new();
        let mut tool_bytes = 0usize;
        let mut tool_calls = ToolCalls::new();
        let mut final_usage: Option<Meters> = None;
        let mut execution = "failed".to_string();
        let mut pending = false;
        let mut last_model = String::new();
        let mut downgrade: Option<Downgrade> = None;
        let mut content_filtered = false;

        run.send(id, "request.started", json!({ "service_version": service.version }))?;
        let max_attempts = usize::try_from(service.max_attempts).unwrap_or(0);
        for candidate in candidates.iter().take(max_attempts) {
            if aborted || cancel.is_cancelled() || timeout.is_elapsed() {
                execution = "cancelled".to_string();
                break;
            }
            let provider = self.providers.get(&candidate.offering.provider).ok_or_else(|| fail("provider_unavailable", 503))?;
            let attempt = match self.store.begin(p, id, &candidate.offering, &candidate.cost.to_string()).await {
                Ok(attempt) => attempt,
                Err(e) if e.is("budget_exceeded") || e.is("connection_disabled") || e.is("cancelled") => {
                    execution = if e.is("cancelled") { "cancelled" } else { "failed" }.to_string();
                    continue;
                }
                Err(e) => return Err(e),
            };
            last_model.clone_from(&candidate.offering.model);
            downgrade = None;
            if candidate.downgrade {
                let d = Downgrade {
                    from_tier: service.tier.clone(),
                    to_tier: service.downgrade_policy.as_ref().map(|d| d.tier.clone()).unwrap_or_default(),
                };
                run.send(
                    id,
                    "routing.downgrade",
                    json!({ "from_tier": d.from_tier, "to_tier": d.to_tier, "offering": candidate.offering.id }),
                )?;
                downgrade = Some(d);
            }

            let mut usage: Option<Meters> = None;
            let mut upstream = String::new();
            let mut done = false;
            let mut completion = "succeeded".to_string();
            let mut filtered = false;
            let mut stream = provider.invoke(input, &candidate.offering);
            let mut call_err: Option<ProviderError> = loop {
                tokio::select! {
                    biased;
                    () = cancel.cancelled() => { aborted = true; break Some(ProviderError::unknown("upstream_failed")); }
                    () = &mut timeout => { aborted = true; break Some(ProviderError::unknown("upstream_failed")); }
                    _ = poll.tick() => {
                        match self.store.get(p, id).await {
                            Ok(rec) if !rec.cancel_requested => {}
                            _ => { aborted = true; break Some(ProviderError::unknown("upstream_failed")); }
                        }
                    }
                    item = stream.next() => match item {
                        None => break None,
                        Some(Err(e)) => break Some(e),
                        Some(Ok(ProviderEvent::Delta { text: delta })) => {
                            if !run.started {
                                if let Err(e) = self.store.output(p, id).await {
                                    break Some(ProviderError::unknown(format!("store: {e}")));
                                }
                                run.started = true;
                            }
                            text.push_str(&delta);
                            if text.len() + tool_bytes > MAX_OUTPUT_BYTES {
                                break Some(ProviderError::unknown("output_too_large"));
                            }
                            if let Err(e) = run.send(id, "output.delta", json!({ "text": delta })) {
                                break Some(ProviderError::unknown(format!("emit: {e}")));
                            }
                        }
                        Some(Ok(ProviderEvent::ToolCall { index, id: call_id, name, signature, arguments_delta })) => {
                            let known = tool_calls.contains_key(&index);
                            let new_ok = tool_calls.len() < MAX_TOOL_CALLS
                                && call_id.as_deref().is_some_and(|s| !s.is_empty())
                                && name.as_deref().is_some_and(|s| !s.is_empty());
                            if input.input.tools.is_none() || index > MAX_SAFE_INDEX || (!known && !new_ok) {
                                break Some(ProviderError::unknown("invalid_tool_call"));
                            }
                            // A tool call is output: after this point a failure is partial and never falls back to
                            // another offering.
                            if !run.started {
                                if let Err(e) = self.store.output(p, id).await {
                                    break Some(ProviderError::unknown(format!("store: {e}")));
                                }
                                run.started = true;
                            }
                            tool_bytes += arguments_delta.len();
                            if text.len() + tool_bytes > MAX_OUTPUT_BYTES {
                                break Some(ProviderError::unknown("output_too_large"));
                            }
                            let data = if let Some(call) = tool_calls.get_mut(&index) {
                                call.arguments.push_str(&arguments_delta);
                                json!({ "index": index, "arguments_delta": arguments_delta })
                            } else {
                                let (call_id, name) = (call_id.unwrap_or_default(), name.unwrap_or_default());
                                let signature = signature.filter(|s| valid_signature(s));
                                let mut data = json!({ "index": index, "id": call_id, "name": name, "arguments_delta": arguments_delta });
                                if let Some(signature) = &signature {
                                    data["signature"] = json!(signature);
                                }
                                tool_calls.insert(index, ToolCall { id: call_id, name, arguments: arguments_delta, signature });
                                data
                            };
                            if let Err(e) = run.send(id, "output.tool_call.delta", data) {
                                break Some(ProviderError::unknown(format!("emit: {e}")));
                            }
                        }
                        Some(Ok(ProviderEvent::Usage { usage: reported, upstream_id })) => {
                            if !reported.is_valid() {
                                break Some(ProviderError::unknown("invalid_usage"));
                            }
                            usage = Some(reported);
                            upstream = upstream_id;
                        }
                        Some(Ok(ProviderEvent::Done { completion: c })) => {
                            done = true;
                            if let Some(c) = c {
                                // "filtered" is a partial completion with a reason: it is stored as partial.
                                filtered = c == "filtered";
                                completion = if filtered { "partial".to_string() } else { c };
                            }
                            break None;
                        }
                    }
                }
            };
            drop(stream);

            if call_err.is_none() {
                match usage.as_ref().filter(|_| done) {
                    None => call_err = Some(ProviderError::unknown("usage_unknown")),
                    Some(u) => {
                        if let Some(code) = check_usage(input, u, &completion) {
                            usage = None;
                            call_err = Some(ProviderError::unknown(code));
                        }
                    }
                }
            }
            if call_err.is_none() {
                if let Some(u) = &usage {
                    match charge(&u.usage(), &candidate.offering.price) {
                        Ok(cost) => {
                            let cost_text = cost.to_string();
                            match self.store.attempt_result(p, id, &attempt, &completion, Some(u), Some(&cost_text), &upstream).await {
                                Ok(()) => {
                                    if completion == "succeeded" {
                                        self.note_health(&candidate.offering, Ok(()));
                                    }
                                    final_usage = usage.clone();
                                    content_filtered = filtered;
                                    execution.clone_from(&completion);
                                    pending = cost > candidate.cost;
                                    break;
                                }
                                Err(e) => call_err = Some(ProviderError::unknown(format!("store: {e}"))),
                            }
                        }
                        Err(e) => call_err = Some(ProviderError::unknown(e.code)),
                    }
                }
            }
            let failure = call_err.unwrap_or_else(|| ProviderError::unknown("usage_unknown"));
            let definitive = failure.definitive && !run.started;
            let cost: Option<String> = if let Some(u) = &usage {
                charge(&u.usage(), &candidate.offering.price).ok().map(|c: BigInt| c.to_string())
            } else if definitive {
                Some("0".to_string())
            } else {
                None
            };
            if definitive {
                self.note_health(&candidate.offering, Err(&failure));
            }
            let status = if definitive { "failed" } else { "unknown" };
            self.store.attempt_result(p, id, &attempt, status, usage.as_ref(), cost.as_deref(), &upstream).await?;
            if !definitive {
                pending = true;
                execution = if run.started { "partial" } else { "unknown" }.to_string();
                break;
            }
            if !failure.retryable {
                break;
            }
        }
        self.store.finish(p, id, &execution, final_usage.as_ref(), pending).await?;
        let settled = self.store.get(p, id).await?;
        // Only a finished response has whole tool calls; a cut-off one must never look callable.
        let mut calls = Vec::new();
        if execution == "succeeded" {
            for (index, mut call) in tool_calls {
                call.arguments = match js_trim(&call.arguments) {
                    "" => "{}".to_string(),
                    trimmed => trimmed.to_string(),
                };
                let mut data = json!({ "index": index, "id": call.id, "name": call.name, "arguments": call.arguments });
                if let Some(signature) = &call.signature {
                    data["signature"] = json!(signature);
                }
                run.send(id, "output.tool_call.completed", data)?;
                calls.push(call);
            }
        }
        let finish_reason = match execution.as_str() {
            "partial" if final_usage.is_some() && content_filtered => Some("content_filter"),
            "partial" if final_usage.is_some() => Some("length"),
            "succeeded" if !calls.is_empty() => Some("tool_calls"),
            "succeeded" => Some("stop"),
            _ => None,
        };
        let mut result = build_result(p, service, legs, request, &settled, &last_model, text, &execution, final_usage);
        if let Some(output) = result.output.as_mut() {
            output.tool_calls = calls;
        }
        result.finish_reason = finish_reason.map(str::to_string);
        let kind = if execution == "succeeded" { "request.completed" } else { "request.failed" };
        let mut data = json!({ "execution_status": execution, "billing_status": settled.billing, "usage": result.usage });
        if let Some(reason) = finish_reason {
            data["finish_reason"] = json!(reason);
        }
        run.send(id, kind, data)?;
        if let Some(d) = &downgrade {
            result.tier.clone_from(&d.to_tier);
        }
        result.downgrade = downgrade;
        Ok(result)
    }
}

#[allow(clippy::too_many_arguments)]
fn build_result(
    p: &Principal,
    service: &Service,
    legs: &[Leg],
    request: &RequestRecord,
    settled: &RequestRecord,
    model: &str,
    text: String,
    execution: &str,
    usage: Option<Meters>,
) -> InferResult {
    let mut res = InferResult {
        downgrade: None,
        replayed: false,
        id: request.id.clone(),
        execution_status: execution.to_string(),
        billing_status: settled.billing.clone(),
        service_version: Some(service.version),
        model: model.to_string(),
        tier: service.tier.clone(),
        output: Some(Output { role: "assistant".into(), content: text, tool_calls: Vec::new() }),
        finish_reason: None,
        usage: None,
        billing: Some(BillingReport { charges: Vec::new() }),
    };
    res.usage = Some(match &usage {
        Some(m) => UsageReport { status: "confirmed".into(), meters: Some(m.clone()) },
        None if settled.billing == "released" => UsageReport { status: "not_billable".into(), meters: None },
        None => UsageReport { status: "pending".into(), meters: None },
    });
    if let (Some(m), "settled") = (&usage, settled.billing.as_str()) {
        for leg in legs {
            if leg.kind != "customer_sale" && !p.has_scope("billing:read") {
                continue;
            }
            if let Ok(due) = charge(&m.usage(), &leg.price) {
                if let Some(b) = res.billing.as_mut() {
                    b.charges.push(Charge { kind: leg.kind.clone(), asset: leg.price.asset.clone(), atoms: due.to_string(), scale: 9 });
                }
            }
        }
    }
    res
}
