//! The PostgreSQL implementation of `super_router_router::DataStore` on this backend's own schema
//! (`migrations/`), plus database-backed API keys and the optional session/weekly usage windows.
//!
//! Queries are plain runtime SQL (no compile-time database), every write runs in a transaction that first takes the
//! tenant's advisory lock, and the migrations are embedded at build time.

mod keys;
mod store;
mod util;
mod windows;

pub use keys::{revalidate_key, KeyRevalidator, Keys};
pub use store::{embedded_migrations, BucketRow, GrantOptions, Options, PostgresStore, Revalidate};
pub use util::iso_millis;
pub use windows::WindowRow;
