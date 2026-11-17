//! The reference in-memory implementation of [`DataStore`].
//!
//! It follows the PostgreSQL store's semantics exactly: idempotent admission, bucketed holds, a provider cost
//! budget, a balanced journal and an append-only outbox. One mutex makes every method atomic. State is lost on
//! restart, which makes it right for tests, demos and single-process self-hosting without a database.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use async_trait::async_trait;
use num_bigint::BigInt;
use num_traits::{Signed, Zero};
use super_router_contracts::{Meters, Offering, Principal, Service};
use super_router_pricing::{atoms, charge};
use super_router_router::{
    system_clock, BalanceRow, Clock, DataStore, Decision, Leg, RequestRecord, Reservation, Store, StoreError, StoreResult,
};
use uuid::Uuid;

fn fail(code: &str, status: u16) -> StoreError {
    StoreError::new(code, status)
}

struct Account {
    id: String,
    owner: String,
    kind: String,
    asset: String,
    balance: BigInt,
    held: BigInt,
}

struct Bucket {
    account_id: String,
    source: String,
    source_kind: String,
    seq: u64,
    /// Milliseconds since the epoch.
    expires_at: Option<i64>,
    remaining: BigInt,
    granted: BigInt,
    held: BigInt,
    /// `None` means every service.
    service_ids: Option<Vec<String>>,
}

struct Allocation {
    bucket: usize,
    account: String,
    amount: BigInt,
    charged: BigInt,
}

struct Attempt {
    id: String,
    status: String,
    usage: Option<Meters>,
    cost: Option<BigInt>,
    bound: BigInt,
}

struct Req {
    record: RequestRecord,
    digest: String,
    provider_spent: BigInt,
    provider_held: BigInt,
    attempts: Vec<Attempt>,
    allocations: Vec<Allocation>,
}

/// One balanced double-entry line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalEntry {
    pub key: String,
    pub asset: String,
    pub debit: String,
    pub credit: String,
    pub amount: String,
    pub reason: String,
}

/// A domain event waiting for delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxEvent {
    pub id: String,
    /// `billing.settled`, `billing.released` or `reconciliation.required`.
    pub kind: String,
    pub request: String,
}

/// A funding bucket as reported by [`MemoryStore::inspect`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BucketView {
    pub source: String,
    pub remaining: String,
    pub held: String,
}

/// A read-only snapshot for tests and diagnostics.
#[derive(Debug, Clone, Default)]
pub struct Inspection {
    pub journal: Vec<JournalEntry>,
    pub outbox: Vec<OutboxEvent>,
    pub reconciliation: HashMap<String, String>,
    pub buckets: Vec<BucketView>,
}

struct Entitlement {
    customer: String,
    service_ids: Vec<String>,
    start: i64,
    end: i64,
}

#[derive(Default)]
struct Tenant {
    services: HashMap<String, Vec<Service>>,
    /// `(owner, kind, asset)` to account id.
    account_index: HashMap<(String, String, String), String>,
    accounts: HashMap<String, Account>,
    buckets: Vec<Bucket>,
    requests: HashMap<String, Req>,
    idem: HashMap<String, String>,
    disabled: HashSet<String>,
    journal: Vec<JournalEntry>,
    outbox: Vec<OutboxEvent>,
    reconciliation: HashMap<String, String>,
    active: HashMap<String, i64>,
    entitlements: Vec<Entitlement>,
}

#[derive(Default)]
struct Inner {
    tenants: HashMap<String, Tenant>,
    seq: u64,
}

/// The optional parts of a grant, used by hosts that sell subscriptions or promotions.
#[derive(Debug, Clone, Default)]
pub struct GrantOptions {
    /// Milliseconds since the epoch.
    pub expires: Option<i64>,
    /// `None` means every service.
    pub service_ids: Option<Vec<String>>,
    /// `purchased` (the default), `promotion` or `subscription`.
    pub source_kind: String,
}

/// [`DataStore`] in memory.
pub struct MemoryStore {
    inner: Mutex<Inner>,
    clock: Clock,
}

impl Default for MemoryStore {
    fn default() -> Self {
        Self::new(None)
    }
}

fn tenant_key(p: &Principal) -> String {
    format!("{}\0{}", p.workspace, p.environment)
}

fn rank(kind: &str) -> u8 {
    match kind {
        "subscription" => 0,
        "promotion" => 1,
        _ => 2,
    }
}

fn expiry(b: &Bucket) -> i64 {
    b.expires_at.unwrap_or(i64::MAX)
}

/// The order buckets are spent in: soonest expiry, then subscription before promotion before purchase, then age.
fn by_expiry(a: &Bucket, b: &Bucket) -> std::cmp::Ordering {
    expiry(a).cmp(&expiry(b)).then_with(|| rank(&a.source_kind).cmp(&rank(&b.source_kind))).then_with(|| a.seq.cmp(&b.seq))
}

fn valid_amount(s: &str) -> bool {
    (1..=30).contains(&s.len()) && !s.starts_with('0') && s.bytes().all(|c| c.is_ascii_digit())
}

fn view(r: &Req) -> RequestRecord {
    RequestRecord { provider_spent: r.provider_spent.to_string(), ..r.record.clone() }
}

fn row<'a>(t: &'a mut Tenant, p: &Principal, id: &str) -> StoreResult<&'a mut Req> {
    let r = t.requests.get_mut(id).filter(|r| r.record.customer == p.customer).ok_or_else(|| fail("resource_not_found", 404))?;
    if !p.may_use(&r.record.service.id) {
        return Err(fail("scope_denied", 403));
    }
    Ok(r)
}

fn same<T: PartialEq>(a: &T, b: &T) -> bool {
    a == b
}

impl MemoryStore {
    /// Creates a store. `None` means the wall clock.
    pub fn new(clock: Option<Clock>) -> Self {
        Self { inner: Mutex::new(Inner::default()), clock: clock.unwrap_or_else(system_clock) }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn now(&self) -> i64 {
        (self.clock)()
    }

    /// Adds a funding bucket with the full set of options.
    pub fn grant_with(&self, p: &Principal, kind: &str, asset: &str, amount: &str, source: &str, opt: GrantOptions) -> StoreResult<String> {
        if !p.has_scope("billing:write") {
            return Err(fail("scope_denied", 403));
        }
        if !valid_amount(amount) {
            return Err(fail("invalid_amount", 400));
        }
        let source_kind = if opt.source_kind.is_empty() { "purchased".to_string() } else { opt.source_kind.clone() };
        let mut guard = self.lock();
        let Inner { tenants, seq } = &mut *guard;
        let t = tenants.entry(tenant_key(p)).or_default();
        let owner = if kind == "platform_sale" { p.workspace.clone() } else { p.customer.clone() };
        if let Some(b) = t.buckets.iter().find(|b| b.source == source) {
            let a = &t.accounts[&b.account_id];
            let same_grant = b.granted == atoms(amount)?
                && a.asset == asset
                && a.owner == owner
                && a.kind == kind
                && b.expires_at == opt.expires
                && b.service_ids == opt.service_ids
                && b.source_kind == source_kind;
            return if same_grant { Ok(a.id.clone()) } else { Err(fail("idempotency_conflict", 409)) };
        }
        let n = atoms(amount)?;
        let index_key = (owner.clone(), kind.to_string(), asset.to_string());
        let account_id = match t.account_index.get(&index_key) {
            Some(id) => id.clone(),
            None => {
                *seq += 1;
                let id = format!("acct_{:08}", *seq);
                t.accounts.insert(
                    id.clone(),
                    Account {
                        id: id.clone(),
                        owner,
                        kind: kind.to_string(),
                        asset: asset.to_string(),
                        balance: BigInt::zero(),
                        held: BigInt::zero(),
                    },
                );
                t.account_index.insert(index_key, id.clone());
                id
            }
        };
        if let Some(a) = t.accounts.get_mut(&account_id) {
            a.balance += &n;
        }
        *seq += 1;
        t.buckets.push(Bucket {
            seq: *seq,
            account_id: account_id.clone(),
            source: source.to_string(),
            source_kind,
            expires_at: opt.expires,
            remaining: n.clone(),
            granted: n,
            held: BigInt::zero(),
            service_ids: opt.service_ids,
        });
        t.journal.push(JournalEntry {
            key: format!("grant:{source}"),
            asset: asset.to_string(),
            debit: format!("issuer:{asset}"),
            credit: account_id.clone(),
            amount: amount.to_string(),
            reason: "grant".to_string(),
        });
        Ok(account_id)
    }

    /// Lets a customer use subscription-only services for a period (milliseconds since the epoch). The control
    /// plane does this in the SQL store.
    pub fn entitle(&self, p: &Principal, service_ids: &[String], start: i64, end: i64) {
        let mut guard = self.lock();
        let t = guard.tenants.entry(tenant_key(p)).or_default();
        t.entitlements.push(Entitlement { customer: p.customer.clone(), service_ids: service_ids.to_vec(), start, end });
    }

    /// A read-only snapshot of a tenant for tests and diagnostics.
    pub fn inspect(&self, p: &Principal) -> Inspection {
        let mut guard = self.lock();
        let t = guard.tenants.entry(tenant_key(p)).or_default();
        Inspection {
            journal: t.journal.clone(),
            outbox: t.outbox.clone(),
            reconciliation: t.reconciliation.clone(),
            buckets: t
                .buckets
                .iter()
                .map(|b| BucketView { source: b.source.clone(), remaining: b.remaining.to_string(), held: b.held.to_string() })
                .collect(),
        }
    }

    /// The ids of a tenant's requests in id order. For tests and diagnostics.
    pub fn request_ids(&self, p: &Principal) -> Vec<String> {
        let mut guard = self.lock();
        let mut ids: Vec<String> = guard.tenants.entry(tenant_key(p)).or_default().requests.keys().cloned().collect();
        ids.sort();
        ids
    }
}

fn event(t: &mut Tenant, request: &str, kind: &str) {
    t.outbox.push(OutboxEvent { id: Uuid::new_v4().to_string(), kind: kind.to_string(), request: request.to_string() });
}

/// Moves the unheld remainder of expired buckets out of the balance.
fn expire(t: &mut Tenant, seq: &mut u64, now: i64) {
    for b in &mut t.buckets {
        if b.expires_at.is_none_or(|e| e > now) || b.remaining <= b.held {
            continue;
        }
        let n = &b.remaining - &b.held;
        b.remaining = b.held.clone();
        if let Some(a) = t.accounts.get_mut(&b.account_id) {
            a.balance -= &n;
            *seq += 1;
            t.journal.push(JournalEntry {
                key: format!("expire:x_{:08}", *seq),
                asset: a.asset.clone(),
                debit: b.account_id.clone(),
                credit: format!("expired:{}", a.asset),
                amount: n.to_string(),
                reason: "expiry".to_string(),
            });
        }
    }
}

fn finish_tx(
    t: &mut Tenant,
    seq: &mut u64,
    now: i64,
    r: &mut Req,
    execution: &str,
    usage: Option<&Meters>,
    mut pending: bool,
) -> StoreResult<()> {
    if r.record.billing == "settled" || r.record.billing == "released" {
        return Ok(());
    }
    let mut execution = execution.to_string();
    if pending && !r.record.output_started && r.attempts.is_empty() {
        pending = false;
        execution = "cancelled".to_string();
    }
    if usage.is_some() && r.attempts.iter().any(|a| a.cost.as_ref().is_some_and(|c| *c > a.bound)) {
        pending = true;
    }
    let mut due = Vec::with_capacity(r.record.legs.len());
    let mut exceeded = false;
    for leg in &r.record.legs {
        let mut amount = BigInt::zero();
        if let Some(u) = usage {
            amount = charge(&u.usage(), &leg.price)?;
            if amount > atoms(&leg.reserved)? {
                exceeded = true;
            }
        }
        due.push(amount);
    }
    if pending || exceeded {
        if r.record.billing != "pending" {
            event(t, &r.record.id, "reconciliation.required");
        }
        r.record.execution = execution;
        r.record.billing = "pending".to_string();
        let reason = if pending { "unknown_outcome" } else { "reservation_exceeded" };
        t.reconciliation.entry(r.record.id.clone()).or_insert_with(|| reason.to_string());
        return Ok(());
    }
    for (leg, amount) in r.record.legs.iter().zip(&due) {
        let mut remaining = amount.clone();
        let mut own: Vec<usize> = (0..r.allocations.len()).filter(|i| r.allocations[*i].account == leg.account).collect();
        own.sort_by(|x, y| by_expiry(&t.buckets[r.allocations[*x].bucket], &t.buckets[r.allocations[*y].bucket]));
        for i in own {
            let a = &mut r.allocations[i];
            let take = if a.amount < remaining { a.amount.clone() } else { remaining.clone() };
            remaining -= &take;
            a.charged = take.clone();
            let b = &mut t.buckets[a.bucket];
            b.remaining -= &take;
            b.held -= &a.amount;
        }
        if let Some(a) = t.accounts.get_mut(&leg.account) {
            a.balance -= amount;
            a.held -= atoms(&leg.reserved)?;
        }
        t.journal.push(JournalEntry {
            key: format!("settle:{}:{}", r.record.id, leg.kind),
            asset: leg.price.asset.clone(),
            debit: leg.account.clone(),
            credit: format!("consumed:{}", leg.price.asset),
            amount: amount.to_string(),
            reason: "settlement".to_string(),
        });
    }
    r.record.execution = execution;
    let kind = if usage.is_some() { "billing.settled" } else { "billing.released" };
    r.record.billing = if usage.is_some() { "settled" } else { "released" }.to_string();
    expire(t, seq, now);
    event(t, &r.record.id, kind);
    Ok(())
}

#[async_trait]
impl Store for MemoryStore {
    async fn replay(&self, p: &Principal, key: &str, digest: &str) -> StoreResult<Option<RequestRecord>> {
        let mut guard = self.lock();
        let t = guard.tenants.entry(tenant_key(p)).or_default();
        let Some(id) = t.idem.get(key) else { return Ok(None) };
        let r = &t.requests[id];
        if r.digest != digest || r.record.customer != p.customer {
            return Err(fail("idempotency_conflict", 409));
        }
        Ok(Some(view(r)))
    }

    async fn get_service(&self, p: &Principal, id: &str, version: Option<i64>) -> StoreResult<Service> {
        if !p.may_use(id) {
            return Err(fail("scope_denied", 403));
        }
        let mut guard = self.lock();
        let t = guard.tenants.entry(tenant_key(p)).or_default();
        let wanted = version.or_else(|| t.active.get(id).copied());
        wanted
            .and_then(|w| t.services.get(id).and_then(|versions| versions.iter().find(|v| v.version == w)))
            .cloned()
            .ok_or_else(|| fail("resource_not_found", 404))
    }

    async fn accounts(&self, p: &Principal, service: &Service) -> StoreResult<Vec<Leg>> {
        let mut guard = self.lock();
        let t = guard.tenants.entry(tenant_key(p)).or_default();
        let mut legs = Vec::new();
        for (kind, price) in [("customer_sale", Some(&service.price)), ("platform_sale", service.platform_price.as_ref())] {
            let Some(price) = price else { continue };
            let owner = if kind == "platform_sale" { &p.workspace } else { &p.customer };
            let id = t
                .account_index
                .get(&(owner.clone(), kind.to_string(), price.asset.clone()))
                .ok_or_else(|| fail("insufficient_balance", 402))?;
            legs.push(Leg { kind: kind.to_string(), account: id.clone(), price: price.clone(), reserved: "0".to_string() });
        }
        Ok(legs)
    }

    async fn reserve(
        &self,
        p: &Principal,
        key: &str,
        digest: &str,
        service: &Service,
        legs: &[Leg],
        deadline: i64,
        _decision: &Decision,
    ) -> StoreResult<Reservation> {
        if !p.may_use(&service.id) {
            return Err(fail("scope_denied", 403));
        }
        let now = self.now();
        let mut guard = self.lock();
        let t = guard.tenants.entry(tenant_key(p)).or_default();
        if service.requires_subscription == Some(true) {
            let entitled = t
                .entitlements
                .iter()
                .any(|e| e.customer == p.customer && e.service_ids.contains(&service.id) && e.start <= now && now < e.end);
            if !entitled {
                return Err(fail("subscription_required", 403));
            }
        }
        if let Some(old_id) = t.idem.get(key) {
            let old = &t.requests[old_id];
            if old.digest != digest || old.record.customer != p.customer {
                return Err(fail("idempotency_conflict", 409));
            }
            return Ok(Reservation { request: view(old), fresh: false });
        }
        if t.active.get(&service.id) != Some(&service.version) {
            return Err(fail("inactive_service_version", 409));
        }
        // Compute every allocation first, then apply: a failure leaves no partial hold.
        let mut plan: Vec<(usize, BigInt)> = Vec::new();
        let mut held_now: HashMap<usize, BigInt> = HashMap::new();
        let mut sorted: Vec<&Leg> = legs.iter().collect();
        sorted.sort_by(|a, b| a.account.cmp(&b.account));
        for leg in sorted {
            let mut remaining = atoms(&leg.reserved)?;
            if !t.accounts.contains_key(&leg.account) {
                return Err(fail("insufficient_balance", 402));
            }
            let mut candidates: Vec<usize> = (0..t.buckets.len())
                .filter(|i| {
                    let b = &t.buckets[*i];
                    b.account_id == leg.account
                        && b.expires_at.is_none_or(|e| e > now)
                        && b.service_ids.as_ref().is_none_or(|ids| ids.contains(&service.id))
                })
                .collect();
            candidates.sort_by(|x, y| by_expiry(&t.buckets[*x], &t.buckets[*y]));
            for i in candidates {
                let b = &t.buckets[i];
                let mut available = &b.remaining - &b.held;
                if let Some(h) = held_now.get(&i) {
                    available -= h;
                }
                let take = if available >= remaining { remaining.clone() } else { available };
                if !take.is_positive() {
                    continue;
                }
                *held_now.entry(i).or_default() += &take;
                remaining -= &take;
                plan.push((i, take));
                if remaining.is_zero() {
                    break;
                }
            }
            if remaining.is_positive() {
                return Err(fail("insufficient_balance", 402));
            }
        }
        let id = Uuid::new_v4().to_string();
        let record = RequestRecord {
            id: id.clone(),
            workspace: p.workspace.clone(),
            environment: p.environment.clone(),
            customer: p.customer.clone(),
            service: service.clone(),
            execution: "accepted".to_string(),
            billing: "reserved".to_string(),
            cancel_requested: false,
            output_started: false,
            deadline,
            legs: legs.to_vec(),
            provider_spent: "0".to_string(),
        };
        let mut allocations = Vec::with_capacity(plan.len());
        for (i, amount) in plan {
            t.buckets[i].held += &amount;
            allocations.push(Allocation { bucket: i, account: t.buckets[i].account_id.clone(), amount, charged: BigInt::zero() });
        }
        for leg in legs {
            if let Some(a) = t.accounts.get_mut(&leg.account) {
                a.held += atoms(&leg.reserved)?;
            }
        }
        let req = Req {
            record,
            digest: digest.to_string(),
            provider_spent: BigInt::zero(),
            provider_held: BigInt::zero(),
            attempts: Vec::new(),
            allocations,
        };
        let request = view(&req);
        t.requests.insert(id.clone(), req);
        t.idem.insert(key.to_string(), id);
        Ok(Reservation { request, fresh: true })
    }

    async fn get(&self, p: &Principal, id: &str) -> StoreResult<RequestRecord> {
        let mut guard = self.lock();
        let t = guard.tenants.entry(tenant_key(p)).or_default();
        Ok(view(row(t, p, id)?))
    }

    async fn begin(&self, p: &Principal, id: &str, offering: &Offering, bound: &str) -> StoreResult<String> {
        let now = self.now();
        let mut guard = self.lock();
        let t = guard.tenants.entry(tenant_key(p)).or_default();
        let disabled = t.disabled.contains(&offering.id);
        let r = row(t, p, id)?;
        if r.record.cancel_requested || now > r.record.deadline {
            return Err(fail("cancelled", 409));
        }
        if r.record.billing != "reserved" {
            return Err(fail("state_conflict", 409));
        }
        if disabled {
            return Err(fail("connection_disabled", 409));
        }
        let snapshot = r.record.service.all_offerings().into_iter().rfind(|o| o.id == offering.id);
        if !snapshot.is_some_and(|s| same(s, offering)) {
            return Err(fail("state_conflict", 409));
        }
        let bound = atoms(bound)?;
        if &r.provider_spent + &r.provider_held + &bound > atoms(&r.record.service.max_provider_cost)? {
            return Err(fail("budget_exceeded", 402));
        }
        let attempt =
            Attempt { id: Uuid::new_v4().to_string(), status: "dispatched".to_string(), usage: None, cost: None, bound: bound.clone() };
        let attempt_id = attempt.id.clone();
        r.attempts.push(attempt);
        r.record.execution = "running".to_string();
        r.provider_held += bound;
        Ok(attempt_id)
    }

    async fn output(&self, p: &Principal, id: &str) -> StoreResult<()> {
        let mut guard = self.lock();
        let t = guard.tenants.entry(tenant_key(p)).or_default();
        row(t, p, id)?.record.output_started = true;
        Ok(())
    }

    async fn attempt_result(
        &self,
        p: &Principal,
        id: &str,
        attempt: &str,
        status: &str,
        usage: Option<&Meters>,
        cost: Option<&str>,
        _upstream: &str,
    ) -> StoreResult<()> {
        let mut guard = self.lock();
        let t = guard.tenants.entry(tenant_key(p)).or_default();
        let r = row(t, p, id)?;
        if r.record.billing != "reserved" && r.record.billing != "pending" {
            return Err(fail("state_conflict", 409));
        }
        let Some(a) = r.attempts.iter_mut().rfind(|a| a.id == attempt) else {
            return Err(fail("resource_not_found", 404));
        };
        // The first recorded result of an attempt is final.
        if a.status != "dispatched" {
            return Ok(());
        }
        a.status = status.to_string();
        a.usage = usage.cloned();
        if let Some(cost) = cost {
            let cost = atoms(cost)?;
            r.provider_spent += &cost;
            r.provider_held -= &a.bound;
            a.cost = Some(cost);
        }
        Ok(())
    }

    async fn finish(&self, p: &Principal, id: &str, execution: &str, usage: Option<&Meters>, pending: bool) -> StoreResult<()> {
        let now = self.now();
        let mut guard = self.lock();
        let Inner { tenants, seq } = &mut *guard;
        let t = tenants.entry(tenant_key(p)).or_default();
        row(t, p, id)?;
        let mut r = t.requests.remove(id).ok_or_else(|| fail("resource_not_found", 404))?;
        let out = finish_tx(t, seq, now, &mut r, execution, usage, pending);
        t.requests.insert(id.to_string(), r);
        out
    }

    async fn cancel(&self, p: &Principal, id: &str) -> StoreResult<()> {
        let mut guard = self.lock();
        let t = guard.tenants.entry(tenant_key(p)).or_default();
        row(t, p, id)?.record.cancel_requested = true;
        Ok(())
    }
}

#[async_trait]
impl DataStore for MemoryStore {
    async fn publish(&self, p: &Principal, service: &Service, mode: &str, expected_version: Option<i64>) -> StoreResult<Service> {
        if !p.has_scope("service:write") || !p.may_use(&service.id) {
            return Err(fail("scope_denied", 403));
        }
        let mode = if mode.is_empty() { "self-host" } else { mode };
        if mode == "cloud" && service.all_offerings().iter().any(|o| o.source == "byok") {
            return Err(fail("byok_not_allowed", 403));
        }
        let mut guard = self.lock();
        let t = guard.tenants.entry(tenant_key(p)).or_default();
        let versions = t.services.entry(service.id.clone()).or_default();
        let previous = versions.last();
        if expected_version.is_some() && previous.map(|v| v.version) != expected_version {
            return Err(fail("service_version_conflict", 409));
        }
        if mode == "cloud" && !p.auth_key_id.is_empty() {
            let protected = previous
                .is_some_and(|prev| same(&prev.platform_price, &service.platform_price) && prev.all_offerings() == service.all_offerings());
            if !protected {
                return Err(fail("platform_pricing_protected", 403));
            }
        }
        if let Some(existing) = versions.iter().find(|v| v.version == service.version) {
            if !same(existing, service) {
                return Err(fail("version_exists", 409));
            }
            return Ok(service.clone());
        }
        if previous.is_some_and(|prev| service.version <= prev.version) {
            return Err(fail("service_version_conflict", 409));
        }
        versions.push(service.clone());
        t.active.insert(service.id.clone(), service.version);
        Ok(service.clone())
    }

    async fn list(&self, p: &Principal) -> StoreResult<Vec<Service>> {
        let mut guard = self.lock();
        let t = guard.tenants.entry(tenant_key(p)).or_default();
        let mut ids: Vec<&String> = t.active.keys().collect();
        ids.sort();
        ids.truncate(100);
        Ok(ids
            .into_iter()
            .filter(|id| p.may_use(id))
            .flat_map(|id| t.services[id].iter().filter(|v| Some(&v.version) == t.active.get(id)).cloned())
            .collect())
    }

    async fn balance(&self, p: &Principal) -> StoreResult<Vec<BalanceRow>> {
        let now = self.now();
        let mut guard = self.lock();
        let Inner { tenants, seq } = &mut *guard;
        let t = tenants.entry(tenant_key(p)).or_default();
        expire(t, seq, now);
        let mut accounts: Vec<&Account> =
            t.accounts.values().filter(|a| a.owner == p.customer || (a.owner == p.workspace && a.kind == "platform_sale")).collect();
        accounts.sort_by(|a, b| a.kind.cmp(&b.kind).then_with(|| a.id.cmp(&b.id)));
        Ok(accounts
            .into_iter()
            .map(|a| BalanceRow {
                id: a.id.clone(),
                kind: a.kind.clone(),
                asset: a.asset.clone(),
                balance: a.balance.to_string(),
                held: a.held.to_string(),
                available: (&a.balance - &a.held).to_string(),
            })
            .collect())
    }

    async fn grant(&self, p: &Principal, kind: &str, asset: &str, amount: &str, source: &str, expires: Option<i64>) -> StoreResult<String> {
        self.grant_with(p, kind, asset, amount, source, GrantOptions { expires, ..GrantOptions::default() })
    }

    async fn disable(&self, p: &Principal, offering: &str) -> StoreResult<()> {
        if !p.has_scope("service:write") {
            return Err(fail("scope_denied", 403));
        }
        let mut guard = self.lock();
        guard.tenants.entry(tenant_key(p)).or_default().disabled.insert(offering.to_string());
        Ok(())
    }

    /// Never redispatches an expired uncertain attempt.
    async fn recover(&self) -> StoreResult<usize> {
        let now = self.now();
        let mut guard = self.lock();
        let Inner { tenants, seq } = &mut *guard;
        let mut processed = 0;
        for t in tenants.values_mut() {
            let mut ids: Vec<String> = t.requests.keys().cloned().collect();
            ids.sort();
            for id in ids {
                let Some(mut r) = t.requests.remove(&id) else { continue };
                if r.record.billing != "reserved" || r.record.deadline >= now {
                    t.requests.insert(id, r);
                    continue;
                }
                let last = r.attempts.last();
                let known = last.is_some_and(|a| (a.status == "succeeded" || a.status == "partial") && a.usage.is_some());
                let dispatched = last.is_some();
                let (execution, usage) = match last {
                    Some(a) if known => (a.status.clone(), a.usage.clone()),
                    Some(_) => ("unknown".to_string(), None),
                    None => ("cancelled".to_string(), None),
                };
                let out = finish_tx(t, seq, now, &mut r, &execution, usage.as_ref(), dispatched && !known);
                t.requests.insert(id, r);
                out?;
                processed += 1;
            }
        }
        Ok(processed)
    }
}
