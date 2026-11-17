//! The data-plane conformance scenarios and the concurrency invariants on PostgreSQL.

mod common;

use std::sync::Arc;
use std::time::Duration;

use super_router_contracts::Principal;
use super_router_dataplane_test::{concurrency, run_all, BucketRow, Harness};
use super_router_router::{system_clock, DataStore};
use super_router_store_postgres::PostgresStore;

fn harness(store: Arc<PostgresStore>) -> Harness {
    let inspect = store.clone();
    Harness {
        store: store as Arc<dyn DataStore>,
        clock: system_clock(),
        advance: Box::new(|ms| Box::pin(async move { tokio::time::sleep(Duration::from_millis(ms.unsigned_abs())).await })),
        buckets: Box::new(move |p: Principal| {
            let store = inspect.clone();
            Box::pin(async move {
                let rows = store.buckets(&p).await.expect("buckets");
                rows.into_iter().map(|b| BucketRow { source: b.source, remaining: b.remaining, held: b.held }).collect()
            })
        }),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scenarios_match_the_reference() {
    let Some(store) = common::open().await else { return };
    let failures = run_all(|| {
        let store = store.clone();
        async move {
            // Recovery is global: settle what earlier runs left expired so a scenario counts only its own.
            store.recover().await.expect("recover");
            harness(store)
        }
    })
    .await;
    assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.join("\n"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_requests_never_overspend() {
    let Some(store) = common::open().await else { return };
    concurrency(harness(store)).await.unwrap();
}

/// The tenant lock is what makes admission idempotent under a race: without it two requests with one key both pass
/// the "seen before?" check and one dies on the unique index.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_same_key_from_many_clients_runs_once() {
    use std::collections::HashMap;

    use super_router_contracts::Provider;
    use super_router_router::MockProvider;
    use tokio_util::sync::CancellationToken;

    let Some(store) = common::open().await else { return };
    let p = common::principal(&format!("race-{}", uuid_like()), "c1", &["invoke", "service:write", "billing:write", "billing:read"]);
    store.publish(&p, &common::conformance_service("chat"), "", None).await.unwrap();
    store.grant(&p, "customer_sale", "credit:demo", "100000", "g1", None).await.unwrap();
    let mock = Arc::new(MockProvider::new("success").with_delay(Duration::from_millis(300)));
    let providers: HashMap<String, Arc<dyn Provider>> = HashMap::from([("ok".to_string(), mock.clone() as Arc<dyn Provider>)]);
    let router = Arc::new(super_router_router::Router::new(store.clone(), providers, super_router_dataplane_test::DIGEST_KEY).unwrap());
    let body = Arc::new(serde_json::to_vec(&super_router_dataplane_test::load()["invocation"]).unwrap());
    let tasks: Vec<_> = (0..8)
        .map(|_| {
            let (router, p, body) = (router.clone(), p.clone(), body.clone());
            tokio::spawn(async move { router.infer(&p, &body, "one-key", None, CancellationToken::new()).await })
        })
        .collect();
    let mut fresh = 0;
    let mut ids = std::collections::BTreeSet::new();
    for t in tasks {
        let r = t.await.unwrap().unwrap_or_else(|e| panic!("a racing duplicate failed: {e}"));
        fresh += usize::from(!r.replayed);
        ids.insert(r.id);
    }
    assert_eq!((fresh, ids.len(), mock.calls()), (1, 1, 1), "one admission, one request id, one provider call");
    let rows = store.balance(&p).await.unwrap();
    assert_eq!((rows[0].balance.as_str(), rows[0].held.as_str()), ("99965", "0"));
}

/// Publishing one version from many writers at once is idempotent: every writer sees success, one row exists.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_publishes_of_one_version_agree() {
    let Some(store) = common::open().await else { return };
    let p = common::principal(&format!("pub-{}", uuid_like()), "c1", &["service:write"]);
    let service = common::conformance_service("chat");
    let tasks: Vec<_> = (0..8)
        .map(|_| {
            let (store, p, service) = (store.clone(), p.clone(), service.clone());
            tokio::spawn(async move { store.publish(&p, &service, "", None).await })
        })
        .collect();
    for t in tasks {
        t.await.unwrap().unwrap_or_else(|e| panic!("a racing publish failed: {e}"));
    }
    let rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM sr_services WHERE workspace=$1").bind(&p.workspace).fetch_one(store.pool()).await.unwrap();
    assert_eq!(rows, 1);
    let audit: i64 =
        sqlx::query_scalar("SELECT count(*) FROM sr_audit WHERE workspace=$1").bind(&p.workspace).fetch_one(store.pool()).await.unwrap();
    assert_eq!(audit, 1, "only the writer that changed something is audited");
}

fn uuid_like() -> String {
    format!("{:x}{:x}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos())
}
