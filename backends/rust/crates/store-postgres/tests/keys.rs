//! Database-backed API keys, ported from the Go `keys_test.go`.

mod common;

use std::sync::Arc;

use sha2::{Digest, Sha256};
use sqlx::PgPool;
use super_router_contracts::Principal;
use super_router_router::{DataStore, StoreError};
use super_router_store_postgres::{KeyRevalidator, Keys, Options, PostgresStore};
use uuid::Uuid;

struct Seeded {
    pool: PgPool,
    ws: String,
    keys: Keys,
    owner_key: String,
    owner_id: String,
    app_token: String,
}

fn token() -> String {
    format!("sr_{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

async fn exec(pool: &PgPool, sql: &str, binds: &[&str]) {
    let mut q = sqlx::query(sql);
    for b in binds {
        q = q.bind(*b);
    }
    q.execute(pool).await.unwrap_or_else(|e| panic!("{sql}: {e}"));
}

async fn seed(store: &PostgresStore) -> Seeded {
    let pool = store.pool().clone();
    let ws = format!("ws-{}", Uuid::new_v4());
    for (id, role) in [("owner1", "owner"), ("app1", "app"), ("view1", "viewer")] {
        exec(&pool, "INSERT INTO sr_members(workspace,environment,id,role) VALUES($1,'test',$2,$3)", &[&ws, id, role]).await;
    }
    exec(&pool, "INSERT INTO sr_customers(workspace,environment,id,external_id) VALUES($1,'test','c1','ext1')", &[&ws]).await;
    let insert = |member: &'static str, customer: &'static str, scopes: &'static str, services: Option<&'static str>| {
        let (pool, ws) = (pool.clone(), ws.clone());
        async move {
            let (tok, id) = (token(), Uuid::new_v4().to_string());
            let hash = hex::encode(Sha256::digest(tok.as_bytes()));
            sqlx::query(
                "INSERT INTO sr_keys(workspace,environment,id,member_id,customer,hash,scopes,service_ids,expires_at) \
                 VALUES($1,'test',$2,$3,$4,$5,$6::text[],$7::text[],now()+interval '1 day')",
            )
            .bind(&ws)
            .bind(&id)
            .bind(member)
            .bind(customer)
            .bind(hash)
            .bind(scopes)
            .bind(services)
            .execute(&pool)
            .await
            .unwrap();
            (tok, id)
        }
    };
    let (owner_key, owner_id) = insert("owner1", "_admin", "{invoke,service:write,billing:write,identity:write}", None).await;
    // billing:write is beyond the app role.
    let (app_token, _) = insert("app1", "c1", "{invoke,billing:write}", Some("{chat}")).await;
    insert("view1", "_admin", "{billing:read}", None).await;
    Seeded { keys: Keys::new(pool.clone()), pool, ws, owner_key, owner_id, app_token }
}

fn invalid_key(err: &StoreError) -> bool {
    err.router().is_some_and(|e| e.code == "invalid_key" && e.status == 401)
}

#[tokio::test]
async fn keys_authenticate() {
    let Some(store) = common::open().await else { return };
    let s = seed(&store).await;
    let p = s.keys.authenticate(&s.owner_key).await.unwrap();
    assert_eq!(
        (p.workspace.as_str(), p.customer.as_str(), p.auth_key_id.as_str(), p.member_id.as_str()),
        (s.ws.as_str(), "_admin", s.owner_id.as_str(), "owner1")
    );
    assert_eq!(p.service_ids, None);
    assert_eq!(p.environment, "test");

    let app = s.keys.authenticate(&s.app_token).await.unwrap();
    // The key lists billing:write but the app role does not allow it, so it is dropped.
    assert_eq!(app.scopes, ["invoke"]);
    assert_eq!(app.service_ids, Some(vec!["chat".to_string()]));
    assert_eq!(app.customer, "c1");

    for (name, tok) in [("unknown", token()), ("malformed", "sr_nope".to_string()), ("empty", String::new())] {
        let err = s.keys.authenticate(&tok).await.unwrap_err();
        assert!(invalid_key(&err), "{name}: {err:?}");
    }
}

#[tokio::test]
async fn keys_refuse_revoked_expired_and_inactive() {
    let Some(store) = common::open().await else { return };
    let s = seed(&store).await;
    let check = |label: &'static str| {
        let (keys, tok) = (s.keys.clone(), s.owner_key.clone());
        async move {
            let err = keys.authenticate(&tok).await.expect_err(label);
            assert!(invalid_key(&err), "{label}: {err:?}");
        }
    };
    exec(&s.pool, "UPDATE sr_members SET active=false WHERE workspace=$1 AND id='owner1'", &[&s.ws]).await;
    check("inactive member").await;
    exec(&s.pool, "UPDATE sr_members SET active=true WHERE workspace=$1 AND id='owner1'", &[&s.ws]).await;
    s.keys.authenticate(&s.owner_key).await.unwrap();
    exec(&s.pool, "UPDATE sr_keys SET expires_at=now()-interval '1 second' WHERE workspace=$1 AND id=$2", &[&s.ws, &s.owner_id]).await;
    check("expired").await;
    exec(
        &s.pool,
        "UPDATE sr_keys SET expires_at=now()+interval '1 day',revoked_at=now() WHERE workspace=$1 AND id=$2",
        &[&s.ws, &s.owner_id],
    )
    .await;
    check("revoked").await;
}

/// A key that was fine when the request was authenticated must not write after it was revoked or narrowed.
#[tokio::test]
async fn revalidate_stops_writes_after_revocation() {
    let Some(plain) = common::open().await else { return };
    let s = seed(&plain).await;
    let store = common::open_with(Options { revalidate: Some(Arc::new(KeyRevalidator)), ..Options::default() }).await.unwrap();
    let p = s.keys.authenticate(&s.owner_key).await.unwrap();
    store.grant(&p, "customer_sale", "credit:demo", "5", "g1", None).await.unwrap();

    exec(&s.pool, "UPDATE sr_keys SET scopes='{invoke}' WHERE workspace=$1 AND id=$2", &[&s.ws, &s.owner_id]).await;
    let err = store.grant(&p, "customer_sale", "credit:demo", "5", "g2", None).await.unwrap_err();
    assert!(err.is("scope_denied"), "narrowed key wrote: {err:?}");

    exec(&s.pool, "UPDATE sr_keys SET revoked_at=now() WHERE workspace=$1 AND id=$2", &[&s.ws, &s.owner_id]).await;
    let err = store.grant(&p, "customer_sale", "credit:demo", "5", "g3", None).await.unwrap_err();
    assert!(err.is("invalid_key"), "revoked key wrote: {err:?}");

    // A principal that did not come from a key is the host's own and is not revalidated.
    let host = Principal {
        workspace: s.ws.clone(),
        environment: "test".into(),
        customer: "c1".into(),
        scopes: vec!["billing:write".into()],
        ..Principal::default()
    };
    store.grant(&host, "customer_sale", "credit:demo", "5", "g4", None).await.unwrap();
}

/// A revoked key's earlier write attempt must have rolled back entirely.
#[tokio::test]
async fn a_refused_revalidation_changes_nothing() {
    let Some(plain) = common::open().await else { return };
    let s = seed(&plain).await;
    let store = common::open_with(Options { revalidate: Some(Arc::new(KeyRevalidator)), ..Options::default() }).await.unwrap();
    let p = s.keys.authenticate(&s.owner_key).await.unwrap();
    exec(&s.pool, "UPDATE sr_keys SET revoked_at=now() WHERE workspace=$1 AND id=$2", &[&s.ws, &s.owner_id]).await;
    assert!(store.grant(&p, "customer_sale", "credit:demo", "5", "g1", None).await.is_err());
    let buckets: i64 =
        sqlx::query_scalar("SELECT count(*) FROM sr_buckets WHERE workspace=$1").bind(&s.ws).fetch_one(&s.pool).await.unwrap();
    assert_eq!(buckets, 0);
}
