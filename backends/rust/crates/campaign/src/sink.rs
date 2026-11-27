//! The one way this crate gives anything: a [`GrantSink`] the host provides.

use async_trait::async_trait;
use std::fmt;

/// The bucket kind of a grant, as the router's store names it. It decides what is spent first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantKind {
    /// Given away: a rebate, a redeemed code. Spent before anything the customer paid for.
    Promotion,
    /// Paid for: a card sold as a product. Spent last.
    Purchased,
}

impl GrantKind {
    pub fn as_str(self) -> &'static str {
        match self {
            GrantKind::Promotion => "promotion",
            GrantKind::Purchased => "purchased",
        }
    }
}

/// Why a grant is made, so a host can narrate it in its own ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cause {
    ReferralRebate,
    CodeRedeemed,
}

/// One grant. `key` is deterministic: the same fact always produces the same key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    pub customer: String,
    /// In the host's own unit (the sink converts), always positive.
    pub amount: i64,
    pub kind: GrantKind,
    /// Epoch milliseconds, `None` for never.
    pub expires_ms: Option<i64>,
    pub key: String,
    pub cause: Cause,
}

/// A sink could not make the grant. Nothing the caller recorded is kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SinkError(pub String);

impl fmt::Display for SinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SinkError {}

/// Where grants go. An implementation **must be idempotent on `key`**: granting a key it already granted with the
/// same amount is success and changes nothing.
#[async_trait]
pub trait GrantSink: Send + Sync {
    async fn grant(&self, grant: &Grant) -> Result<(), SinkError>;
}

#[cfg(feature = "router")]
/// A sink for any router [`DataStore`]: a grant becomes `DataStore::grant` on the `customer_sale` leg.
///
/// The trait's grant has no bucket kind, so everything lands as the store's default kind; a host that needs
/// [`GrantKind::Promotion`] to be spent first (the PostgreSQL store's `GrantOptions`) writes its own sink.
pub struct DataStoreSink<D: ?Sized> {
    store: std::sync::Arc<D>,
    asset: String,
    atoms_per_unit: i64,
    principal: std::sync::Arc<dyn Fn(&str) -> super_router_contracts::Principal + Send + Sync>,
}

#[cfg(feature = "router")]
impl<D: super_router_router::DataStore + ?Sized> DataStoreSink<D> {
    /// `asset` is the router asset id, `atoms_per_unit` converts the host's unit to atoms, and `principal` builds
    /// the trusted principal (with `billing:write`) for a customer id.
    pub fn new(
        store: std::sync::Arc<D>,
        asset: impl Into<String>,
        atoms_per_unit: i64,
        principal: impl Fn(&str) -> super_router_contracts::Principal + Send + Sync + 'static,
    ) -> Self {
        Self { store, asset: asset.into(), atoms_per_unit, principal: std::sync::Arc::new(principal) }
    }
}

#[cfg(feature = "router")]
#[async_trait]
impl<D: super_router_router::DataStore + ?Sized + 'static> GrantSink for DataStoreSink<D> {
    async fn grant(&self, g: &Grant) -> Result<(), SinkError> {
        let atoms = i128::from(g.amount) * i128::from(self.atoms_per_unit);
        if atoms <= 0 {
            return Err(SinkError("a grant must be positive".into()));
        }
        let p = (self.principal)(&g.customer);
        self.store
            .grant(&p, "customer_sale", &self.asset, &atoms.to_string(), &g.key, g.expires_ms)
            .await
            .map(|_| ())
            .map_err(|e| SinkError(format!("{e:?}")))
    }
}
