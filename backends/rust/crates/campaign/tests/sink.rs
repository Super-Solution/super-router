#![cfg(feature = "router")]
//! The blanket sink on the router's own in-memory store: the grant lands as a funding bucket and a repeated key does
//! not fund twice.

use std::sync::Arc;

use super_router_campaign::{Cause, DataStoreSink, Grant, GrantKind, GrantSink};
use super_router_contracts::Principal;
use super_router_router::DataStore;
use super_router_store_memory::MemoryStore;

fn principal(customer: &str) -> Principal {
    Principal {
        workspace: "ws".into(),
        environment: "main".into(),
        customer: customer.into(),
        scopes: vec!["billing:write".into(), "billing:read".into()],
        auth_key_id: "k".into(),
        member_id: "m".into(),
        service_ids: None,
    }
}

fn grant(key: &str, amount: i64) -> Grant {
    Grant { customer: "alice".into(), amount, kind: GrantKind::Promotion, expires_ms: None, key: key.into(), cause: Cause::CodeRedeemed }
}

#[tokio::test]
async fn a_grant_funds_the_customer_once_per_key_and_converts_the_unit() {
    let store = Arc::new(MemoryStore::new(None));
    let sink = DataStoreSink::new(store.clone(), "credit:test", 10_000_000, principal);
    sink.grant(&grant("campaign:redeem:1", 30)).await.unwrap();
    sink.grant(&grant("campaign:redeem:1", 30)).await.unwrap();
    let rows = store.balance(&principal("alice")).await.unwrap();
    let total: i128 = rows.iter().filter(|r| r.asset == "credit:test").map(|r| r.available.parse::<i128>().unwrap()).sum();
    assert_eq!(total, 300_000_000, "30 units at ten million atoms each, funded once");
    assert!(sink.grant(&grant("campaign:redeem:1", 31)).await.is_err(), "the same key with another amount is refused");
    assert!(sink.grant(&grant("campaign:redeem:2", 0)).await.is_err());
}
