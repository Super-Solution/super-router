//! The data-plane conformance scenarios and the concurrency invariants on the in-memory store.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use super_router_contracts::Principal;
use super_router_dataplane_test::{concurrency, run_all, BucketRow, Harness};
use super_router_router::Clock;
use super_router_store_memory::MemoryStore;

fn harness(virtual_time: bool) -> Harness {
    let now = Arc::new(AtomicI64::new(1_800_000_000_000));
    let clock: Clock = if virtual_time {
        let now = now.clone();
        Arc::new(move || now.load(Ordering::SeqCst))
    } else {
        super_router_router::system_clock()
    };
    let store = Arc::new(MemoryStore::new(Some(clock.clone())));
    let inspect = store.clone();
    Harness {
        store,
        clock,
        advance: Box::new(move |ms| {
            let now = now.clone();
            Box::pin(async move {
                if virtual_time {
                    now.fetch_add(ms, Ordering::SeqCst);
                } else {
                    tokio::time::sleep(std::time::Duration::from_millis(ms.unsigned_abs())).await;
                }
            })
        }),
        buckets: Box::new(move |p: Principal| {
            let inspect = inspect.clone();
            Box::pin(async move {
                let mut rows: Vec<BucketRow> = inspect
                    .inspect(&p)
                    .buckets
                    .into_iter()
                    .map(|b| BucketRow { source: b.source, remaining: b.remaining, held: b.held })
                    .collect();
                rows.sort_by(|a, b| a.source.cmp(&b.source));
                rows
            })
        }),
    }
}

#[tokio::test]
async fn scenarios_match_the_reference() {
    let failures = run_all(|| async { harness(true) }).await;
    assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.join("\n"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_requests_never_overspend() {
    concurrency(harness(false)).await.unwrap();
}
