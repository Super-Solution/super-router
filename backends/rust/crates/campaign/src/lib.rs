//! Campaigns for Super Router, as an extension that sits on top of it (not in its core).
//!
//! The router already has the one primitive every giveaway needs: `grant(customer, asset, amount, source, expires)`,
//! idempotent on `source`. This crate decides **who gets a grant and how much** (a referral rebate, a redeem code)
//! and writes the decision, never the router's tables, through that one call. See the architecture decision
//! "campaigns are an extension on top of the router".
//!
//! - The host reports facts (`attach`, `purchase`, `redeem`, a clock tick) and answers a [`GrantSink`].
//! - Every grant carries a key made of the program and the fact's own id, so a repeated fact or a crash between the
//!   decision and the grant grants once.
//! - The crate owns its tables (`sc_*`, `migrations/`) in whatever schema the pool's `search_path` names, with its
//!   own history table, so it never touches the router's or the host's migrations.
//! - Amounts are integers in the host's own unit; the sink converts to the router's atoms.
//!
//! The pure parts ([`codes`], [`rebate`], [`signals`]) read no clock and no database and share conformance data.

pub mod codes;
mod error;
mod migrate;
pub mod rebate;
mod redeem;
mod referral;
mod settings;
pub mod signals;
mod sink;
mod store;
mod types;

pub use error::Error;
pub use migrate::embedded_migrations;
pub use referral::{Released, Review};
pub use signals::Signal;
pub use sink::{Cause, DataStoreSink, Grant, GrantKind, GrantSink, SinkError};
pub use store::Campaigns;
pub use types::*;
