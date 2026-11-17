//! A database that cannot be reached is an internal failure, never a customer-visible error code.

use std::time::Duration;

use sqlx::postgres::PgPoolOptions;
use super_router_contracts::Principal;
use super_router_router::{DataStore, Store, StoreError};
use super_router_store_postgres::{Options, PostgresStore};

#[tokio::test]
async fn an_unreachable_database_surfaces_as_internal_not_as_a_router_error() {
    let pool = PgPoolOptions::new()
        .acquire_timeout(Duration::from_millis(300))
        .connect_lazy("postgresql://nobody:nothing@127.0.0.1:1/none")
        .unwrap();
    let store = PostgresStore::new(pool, Options::default());
    let p = Principal {
        workspace: "w".into(),
        environment: "e".into(),
        customer: "c".into(),
        scopes: vec!["billing:write".into()],
        ..Principal::default()
    };
    for err in [
        store.balance(&p).await.map(|_| ()).unwrap_err(),
        store.get(&p, "x").await.map(|_| ()).unwrap_err(),
        store.recover().await.map(|_| ()).unwrap_err(),
        store.grant(&p, "customer_sale", "a", "5", "s", None).await.map(|_| ()).unwrap_err(),
    ] {
        assert!(matches!(err, StoreError::Internal(_)), "{err:?}");
    }
    // Refusals that need no database are still the customer's to see.
    let denied = store.grant(&Principal { scopes: vec![], ..p.clone() }, "customer_sale", "a", "5", "s", None).await.unwrap_err();
    assert!(denied.is("scope_denied"));
    let bad = store.grant(&p, "customer_sale", "a", "05", "s", None).await.unwrap_err();
    assert!(bad.is("invalid_amount"));
}
