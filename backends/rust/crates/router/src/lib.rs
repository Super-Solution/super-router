//! The Super Router data plane: idempotent admission, reserve, execute and settle.
//!
//! [`Router::infer`] talks to storage through the [`Store`] port and to models through
//! [`super_router_contracts::Provider`], so any database or transport can host it.

mod credit_rates;
mod mock;
mod router;
mod store;

pub use credit_rates::publish_credit_rate_version;
pub use mock::{MockProvider, MockTaskProvider, MockToolProvider};
pub use router::{BillingReport, Charge, Downgrade, Emit, EmitClosed, InferError, InferResult, Output, Router, UsageReport};
pub use store::{
    system_clock, Authenticator, BalanceRow, Clock, DataStore, Decision, DecisionCandidate, Leg, RequestRecord, Reservation, Store,
    StoreError, StoreResult,
};
