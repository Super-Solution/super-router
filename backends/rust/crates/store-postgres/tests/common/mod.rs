//! Shared setup for the database tests: connect to `DATABASE_URL` (or the local development database), migrate, and
//! skip when it is not reachable unless `SR_REQUIRE_POSTGRES` is set, as CI should.

use std::sync::Arc;
use std::time::Duration;

use super_router_contracts::{parse_service, Principal, Service};
use super_router_store_postgres::{Options, PostgresStore};

pub const LOCAL_URL: &str = "postgresql://superrouter:local-development-only@127.0.0.1:25432/superrouter";

pub fn database_url() -> String {
    std::env::var("DATABASE_URL").ok().filter(|u| !u.is_empty()).unwrap_or_else(|| LOCAL_URL.to_string())
}

/// `None` (after saying why) when the database does not answer and is not required.
pub async fn open_with(opts: Options) -> Option<Arc<PostgresStore>> {
    let required = std::env::var("SR_REQUIRE_POSTGRES").is_ok_and(|v| !v.is_empty());
    let url = database_url();
    let connected = tokio::time::timeout(Duration::from_secs(3), PostgresStore::connect(&url, 40, opts)).await;
    match connected {
        Ok(Ok(store)) => {
            store.migrate().await.expect("migrate");
            Some(Arc::new(store))
        }
        other => {
            let why = match other {
                Ok(Err(e)) => e.to_string(),
                _ => "timed out".to_string(),
            };
            assert!(!required, "postgres required but unreachable: {why}");
            eprintln!("skipping: postgres unreachable (set DATABASE_URL): {why}");
            None
        }
    }
}

pub async fn open() -> Option<Arc<PostgresStore>> {
    open_with(Options::default()).await
}

/// A service of the shared conformance file, parsed the strict way.
#[allow(dead_code)]
pub fn conformance_service(name: &str) -> Service {
    let data = super_router_dataplane_test::load();
    parse_service(&serde_json::to_vec(&data["services"][name]).unwrap()).unwrap()
}

#[allow(dead_code)]
pub fn principal(workspace: &str, customer: &str, scopes: &[&str]) -> Principal {
    Principal {
        workspace: workspace.to_string(),
        environment: "test".to_string(),
        customer: customer.to_string(),
        scopes: scopes.iter().map(ToString::to_string).collect(),
        ..Principal::default()
    }
}
