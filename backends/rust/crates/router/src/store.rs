//! The storage port of the router. Implement [`Store`] and [`DataStore`] to plug in another database, and run
//! the data-plane conformance scenarios against it.

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use super_router_contracts::{LabelEvidence, Meters, Offering, Principal, QualityMetadata, RouterError, Service};
use super_router_pricing::Price;
use super_router_routing::Excluded;

use crate::Downgrade;

/// What a store can fail with: a client-visible [`RouterError`], or an internal failure (a lost connection, a
/// corrupt row) whose text is for logs only and never reaches a customer.
#[derive(Debug, Clone, PartialEq)]
pub enum StoreError {
    Router(RouterError),
    Internal(String),
}

impl StoreError {
    pub fn new(code: &str, status: u16) -> Self {
        Self::Router(RouterError::new(code, status))
    }

    /// The router error when this is one.
    pub fn router(&self) -> Option<&RouterError> {
        match self {
            Self::Router(e) => Some(e),
            Self::Internal(_) => None,
        }
    }

    /// Whether this is the router error `code`.
    pub fn is(&self, code: &str) -> bool {
        self.router().is_some_and(|e| e.code == code)
    }
}

impl From<RouterError> for StoreError {
    fn from(e: RouterError) -> Self {
        Self::Router(e)
    }
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Router(e) => f.write_str(&e.code),
            Self::Internal(m) => write!(f, "internal storage error: {m}"),
        }
    }
}

impl std::error::Error for StoreError {}

pub type StoreResult<T> = Result<T, StoreError>;

/// Returns the current time in milliseconds since the epoch. Stores and the router accept one so tests control time.
pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

/// The wall clock.
pub fn system_clock() -> Clock {
    Arc::new(|| {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
    })
}

/// One account the request charges: the customer's sale account and, optionally, the platform's.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Leg {
    /// `customer_sale` or `platform_sale`.
    pub kind: String,
    pub account: String,
    pub price: Price,
    /// Atoms held before the call.
    pub reserved: String,
}

/// What a store knows about one request. Treat it as read-only.
#[derive(Debug, Clone, PartialEq)]
pub struct RequestRecord {
    pub id: String,
    pub workspace: String,
    pub environment: String,
    pub customer: String,
    pub service: Service,
    /// accepted, running, succeeded, partial, unknown, failed or cancelled.
    pub execution: String,
    /// reserved, pending, settled or released.
    pub billing: String,
    pub cancel_requested: bool,
    pub output_started: bool,
    /// Milliseconds since the epoch.
    pub deadline: i64,
    pub legs: Vec<Leg>,
    pub provider_spent: String,
}

/// The outcome of [`Store::reserve`]. `fresh` is false when the idempotency key was already admitted.
#[derive(Debug, Clone)]
pub struct Reservation {
    pub request: RequestRecord,
    pub fresh: bool,
}

/// Why the router chose its candidates; stores may persist it for audit.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Decision {
    pub excluded: Vec<Excluded>,
    pub candidates: Vec<DecisionCandidate>,
}

/// One routable offering with its bound cost and score.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionCandidate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub downgrade: Option<Downgrade>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality_metadata: Option<QualityMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label_evidence: Option<std::collections::BTreeMap<String, LabelEvidence>>,
    pub id: String,
    pub score: f64,
    pub cost: String,
}

/// The storage port of the router. Every method must be atomic and safe for concurrent use.
#[async_trait]
pub trait Store: Send + Sync {
    /// The request admitted under `key`, `None` when there is none, or `idempotency_conflict` (409) when the key
    /// was used for a different request or customer.
    async fn replay(&self, p: &Principal, key: &str, digest: &str) -> StoreResult<Option<RequestRecord>>;

    async fn get_service(&self, p: &Principal, id: &str, version: Option<i64>) -> StoreResult<Service>;

    /// The legs a request for this service charges, or `insufficient_balance` (402) when an account is missing.
    async fn accounts(&self, p: &Principal, service: &Service) -> StoreResult<Vec<Leg>>;

    /// Admits a request and holds each leg's `reserved` amount, all or nothing.
    #[allow(clippy::too_many_arguments)]
    async fn reserve(
        &self,
        p: &Principal,
        key: &str,
        digest: &str,
        service: &Service,
        legs: &[Leg],
        deadline: i64,
        decision: &Decision,
    ) -> StoreResult<Reservation>;

    async fn get(&self, p: &Principal, id: &str) -> StoreResult<RequestRecord>;

    /// Records a provider dispatch and returns its attempt id.
    async fn begin(&self, p: &Principal, id: &str, offering: &Offering, bound: &str) -> StoreResult<String>;

    /// Marks that the caller has seen output, after which a failure can no longer be refunded.
    async fn output(&self, p: &Principal, id: &str) -> StoreResult<()>;

    #[allow(clippy::too_many_arguments)]
    async fn attempt_result(
        &self,
        p: &Principal,
        id: &str,
        attempt: &str,
        status: &str,
        usage: Option<&Meters>,
        cost: Option<&str>,
        upstream: &str,
    ) -> StoreResult<()>;

    /// Settles or releases the reservation, or leaves it pending when the outcome is uncertain.
    async fn finish(&self, p: &Principal, id: &str, execution: &str, usage: Option<&Meters>, pending: bool) -> StoreResult<()>;

    async fn cancel(&self, p: &Principal, id: &str) -> StoreResult<()>;
}

/// One account as the customer sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BalanceRow {
    pub id: String,
    pub kind: String,
    pub asset: String,
    pub balance: String,
    pub held: String,
    pub available: String,
}

/// Everything the data-plane server and worker need from storage beyond the router's own [`Store`].
#[async_trait]
pub trait DataStore: Store {
    /// Stores a service version. `mode` is `cloud` or `self-host`; `expected_version`, when set, must be the
    /// current version.
    async fn publish(&self, p: &Principal, service: &Service, mode: &str, expected_version: Option<i64>) -> StoreResult<Service>;

    async fn list(&self, p: &Principal) -> StoreResult<Vec<Service>>;

    async fn balance(&self, p: &Principal) -> StoreResult<Vec<BalanceRow>>;

    /// Adds a funding bucket. `source` is its idempotency key; `expires` is milliseconds since the epoch.
    async fn grant(&self, p: &Principal, kind: &str, asset: &str, amount: &str, source: &str, expires: Option<i64>) -> StoreResult<String>;

    async fn disable(&self, p: &Principal, offering: &str) -> StoreResult<()>;

    /// Settles or releases requests whose deadline passed, never redispatching an uncertain attempt. Returns how
    /// many requests it processed.
    async fn recover(&self) -> StoreResult<usize>;
}

/// Turns a bearer token into the principal it grants. Return a [`RouterError`] `invalid_key` (401) for an
/// unknown or revoked key. Implement it to back keys with a database or a token verifier; hosts must only build
/// a [`Principal`] from credentials they verified.
#[async_trait]
pub trait Authenticator: Send + Sync {
    async fn authenticate(&self, token: &str) -> Result<Principal, RouterError>;
}
