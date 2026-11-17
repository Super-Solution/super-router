//! Session and weekly usage windows on subscription credits, ported from the Go `windows_test.go`.

mod common;

use std::collections::HashMap;
use std::sync::Arc;

use super_router_contracts::{Principal, Provider, RouterError};
use super_router_router::{DataStore, MockProvider, Router};
use super_router_store_postgres::{GrantOptions, PostgresStore, WindowRow};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// One chat call reserves 2144 atoms and the mock settles it at 35. "FOUR" leaves room for exactly four
/// reservations: 35k + 2144 <= 2250 holds for k = 0..3.
const CHARGE: u64 = 35;
const FOUR: &str = "2250";
const HOUR_MS: i64 = 3_600_000;

struct Fixture {
    store: Arc<PostgresStore>,
    p: Principal,
    router: Router,
    input: Vec<u8>,
    calls: usize,
}

/// Publishes the conformance chat service, funds the customer through a subscription bucket (100000 credits) whose
/// entitlement carries `limits` (empty: the option is off) and returns a router over a mock.
async fn fixture(limits: &str, behavior: &str) -> Option<Fixture> {
    let store = common::open().await?;
    let data = super_router_dataplane_test::load();
    let service = common::conformance_service("chat");
    let p = common::principal(
        &format!("win-{}", Uuid::new_v4()),
        "c1",
        &["invoke", "service:write", "billing:write", "billing:read", "credits:read"],
    );
    store.publish(&p, &service, "", None).await.unwrap();
    for q in [
        "INSERT INTO sr_customers(workspace,environment,id,external_id) VALUES($1,$2,'c1','c1')",
        "INSERT INTO sr_plans(workspace,environment,id,version,config) VALUES($1,$2,'plan',1,'{}')",
        "INSERT INTO sr_subscriptions(workspace,environment,id,customer,plan_id,plan_version,anchor_at,period_days,status) VALUES($1,$2,'sub','c1','plan',1,now(),30,'active')",
    ] {
        sqlx::query(q).bind(&p.workspace).bind(&p.environment).execute(store.pool()).await.unwrap();
    }
    let now = super_router_router::system_clock()();
    let expires = now + 30 * 24 * HOUR_MS;
    store
        .grant_with(
            &p,
            "customer_sale",
            "credit:demo",
            "100000",
            "subscription:sub:0",
            GrantOptions { expires: Some(expires), service_ids: Some(vec!["chat".into()]), source_kind: "subscription".into() },
        )
        .await
        .unwrap();
    let bucket: String =
        sqlx::query_scalar("SELECT id FROM sr_buckets WHERE workspace=$1 AND environment=$2 AND source='subscription:sub:0'")
            .bind(&p.workspace)
            .bind(&p.environment)
            .fetch_one(store.pool())
            .await
            .unwrap();
    store
        .entitle_with(&p, "sub", 0, &["chat".to_string()], now - HOUR_MS, expires, &bucket, Some(limits).filter(|l| !l.is_empty()))
        .await
        .unwrap();
    let providers: HashMap<String, Arc<dyn Provider>> =
        HashMap::from([("ok".to_string(), Arc::new(MockProvider::new(behavior)) as Arc<dyn Provider>)]);
    let router = Router::new(store.clone(), providers, "window-digest-key-0123456789abcdef").unwrap();
    Some(Fixture { store, p, router, input: serde_json::to_vec(&data["invocation"]).unwrap(), calls: 0 })
}

impl Fixture {
    async fn call(&mut self) -> Result<super_router_router::InferResult, RouterError> {
        self.calls += 1;
        let key = format!("k-{}-{}", self.calls, Uuid::new_v4());
        self.router.infer(&self.p, &self.input, &key, None, CancellationToken::new()).await.map_err(|e| e.error)
    }

    async fn must_call(&mut self) {
        let n = self.calls + 1;
        self.call().await.unwrap_or_else(|e| panic!("call {n}: {e}"));
    }

    async fn refused(&mut self) -> RouterError {
        self.call().await.expect_err("expected a refusal")
    }

    async fn rows(&self) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM sr_usage_windows WHERE workspace=$1")
            .bind(&self.p.workspace)
            .fetch_one(self.store.pool())
            .await
            .unwrap()
    }

    async fn window(&self, kind: &str) -> WindowRow {
        let rows = self.store.windows(&self.p).await.unwrap();
        rows.iter().find(|r| r.window == kind).cloned().unwrap_or_else(|| panic!("no {kind} window in {rows:?}"))
    }

    /// Moves every window of a kind into the past, as if that much time had gone by.
    async fn age(&self, kind: &str, hours: i32) {
        sqlx::query("UPDATE sr_usage_windows SET window_start=window_start-make_interval(hours=>$3),window_end=window_end-make_interval(hours=>$3) WHERE workspace=$1 AND kind=$2")
            .bind(&self.p.workspace)
            .bind(kind)
            .bind(hours)
            .execute(self.store.pool())
            .await
            .unwrap();
    }
}

fn detail(e: &RouterError, key: &str) -> serde_json::Value {
    e.details.as_ref().and_then(|d| d.get(key)).cloned().unwrap_or(serde_json::Value::Null)
}

#[tokio::test]
async fn windows_are_off_by_default() {
    for limits in ["", r#"{"enabled":false,"session":{"hours":5,"credits":"1"}}"#] {
        let Some(mut f) = fixture(limits, "success").await else { return };
        for _ in 0..3 {
            f.must_call().await;
        }
        assert_eq!(f.rows().await, 0, "limits {limits:?} wrote windows");
    }
}

#[tokio::test]
async fn session_window_admits_what_fits_then_refuses() {
    let Some(mut f) = fixture(&format!(r#"{{"enabled":true,"session":{{"hours":5,"credits":"{FOUR}"}}}}"#), "success").await else {
        return;
    };
    for _ in 0..4 {
        f.must_call().await;
    }
    let w = f.window("session").await;
    assert_eq!((w.spent.as_str(), w.held.as_str(), w.limit.as_str(), w.hours), ("140", "0", FOUR, 5), "{w:?}");
    assert_eq!(w.resets_at.unwrap() - w.window_start.unwrap(), 5 * HOUR_MS);
    let re = f.refused().await;
    assert_eq!((re.code.as_str(), re.status, re.retryable), ("window_limit_reached", 429, true), "{re:?}");
    assert_eq!(detail(&re, "window"), "session");
    assert_eq!(detail(&re, "limit"), FOUR);
    assert_eq!(detail(&re, "used"), (CHARGE * 4).to_string());
    assert!(detail(&re, "resets_at").is_string(), "an open window tells when it resets");
    // The refusal reserved nothing.
    let held: String = sqlx::query_scalar("SELECT held::text FROM sr_accounts WHERE workspace=$1")
        .bind(&f.p.workspace)
        .fetch_one(f.store.pool())
        .await
        .unwrap();
    assert_eq!(held, "0");
    // A new window opens at the first request after the old one closed.
    f.age("session", 6).await;
    f.must_call().await;
    assert_eq!(f.window("session").await.spent, CHARGE.to_string());
}

#[tokio::test]
async fn weekly_window_binds_independently() {
    // The session fits four calls; the week fits eight (35k + 2144 <= 2400 for k = 0..7).
    let Some(mut f) =
        fixture(&format!(r#"{{"enabled":true,"session":{{"hours":5,"credits":"{FOUR}"}},"weekly":{{"credits":"2400"}}}}"#), "success")
            .await
    else {
        return;
    };
    for _ in 0..4 {
        f.must_call().await;
    }
    assert_eq!(detail(&f.refused().await, "window"), "session");
    f.age("session", 6).await;
    for _ in 0..4 {
        f.must_call().await;
    }
    let window = detail(&f.refused().await, "window");
    assert!(window == "session" || window == "weekly", "{window}");
    f.age("session", 6).await;
    assert_eq!(detail(&f.refused().await, "window"), "weekly", "expected the week to bind");
    f.age("weekly", 169).await;
    f.must_call().await;
}

#[tokio::test]
async fn a_limit_below_one_reservation_is_not_retryable() {
    let Some(mut f) = fixture(r#"{"enabled":true,"session":{"hours":5,"credits":"1000"}}"#, "success").await else { return };
    let re = f.refused().await;
    assert_eq!((re.code.as_str(), re.retryable), ("window_limit_reached", false), "{re:?}");
    assert!(detail(&re, "resets_at").is_null());
    assert_eq!(f.rows().await, 0, "a refused request opened a window");
}

#[tokio::test]
async fn a_full_window_falls_through_to_purchased_credits() {
    let Some(mut f) = fixture(&format!(r#"{{"enabled":true,"session":{{"hours":5,"credits":"{FOUR}"}}}}"#), "success").await else {
        return;
    };
    f.store.grant(&f.p, "customer_sale", "credit:demo", "100000", "pack", None).await.unwrap();
    // 75 calls cost 2625: the window takes exactly its 2250 and the pack the other 375, including the call whose
    // hold straddles both buckets.
    for _ in 0..75 {
        f.must_call().await;
    }
    let w = f.window("session").await;
    assert_eq!((w.spent.as_str(), w.held.as_str()), ("2250", "0"), "{w:?}");
    for b in f.store.buckets(&f.p).await.unwrap() {
        let want = if b.source == "pack" { "99625" } else { "97750" };
        assert_eq!((b.remaining.as_str(), b.held.as_str()), (want, "0"), "{}", b.source);
    }
}

#[tokio::test]
async fn a_released_request_does_not_start_the_clock() {
    let Some(mut f) = fixture(&format!(r#"{{"enabled":true,"session":{{"hours":5,"credits":"{FOUR}"}}}}"#), "reject").await else { return };
    if let Ok(res) = f.call().await {
        assert_eq!(res.billing_status, "released");
    }
    assert_eq!(f.rows().await, 0, "a released request left a window behind");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_calls_cannot_overshoot_a_window() {
    let Some(f) = fixture(&format!(r#"{{"enabled":true,"session":{{"hours":5,"credits":"{FOUR}"}}}}"#), "success").await else { return };
    let f = Arc::new(f);
    let tasks: Vec<_> = (0..12)
        .map(|i| {
            let f = f.clone();
            tokio::spawn(async move { f.router.infer(&f.p, &f.input, &format!("c-{i}"), None, CancellationToken::new()).await })
        })
        .collect();
    let mut ok = 0u64;
    for t in tasks {
        match t.await.unwrap() {
            Ok(_) => ok += 1,
            Err(e) => assert_eq!(e.error.code, "window_limit_reached", "unexpected failure: {e}"),
        }
    }
    assert!((1..=4).contains(&ok), "admitted {ok}");
    let w = f.window("session").await;
    assert_eq!((w.spent, w.held.as_str()), ((CHARGE * ok).to_string(), "0"), "{ok} admitted");
}

#[tokio::test]
async fn the_latest_entitlement_part_governs_the_cycle() {
    let Some(mut f) = fixture(&format!(r#"{{"enabled":true,"session":{{"hours":5,"credits":"{FOUR}"}}}}"#), "success").await else {
        return;
    };
    for _ in 0..4 {
        f.must_call().await;
    }
    f.refused().await;
    let (pool, ws) = (f.store.pool().clone(), f.p.workspace.clone());
    let part = |limits: Option<&'static str>| {
        let (pool, ws) = (pool.clone(), ws.clone());
        async move {
            sqlx::query(
                "INSERT INTO sr_entitlements(workspace,environment,subscription,cycle,customer,service_ids,period_start,period_end,bucket,part,window_limits) \
                 SELECT workspace,environment,subscription,cycle,customer,service_ids,period_start,period_end,bucket,(SELECT max(part)+1 FROM sr_entitlements WHERE workspace=$1),$2::jsonb \
                 FROM sr_entitlements WHERE workspace=$1 AND part=0",
            )
            .bind(ws)
            .bind(limits)
            .execute(&pool)
            .await
            .unwrap();
        }
    };
    part(Some(r#"{"enabled":true,"session":{"hours":5,"credits":"4500"}}"#)).await;
    f.must_call().await;
    let w = f.window("session").await;
    assert_eq!((w.limit.as_str(), w.spent.as_str()), ("4500", (CHARGE * 5).to_string().as_str()), "{w:?}");
    part(None).await;
    assert!(f.store.windows(&f.p).await.unwrap().is_empty());
    for _ in 0..3 {
        f.must_call().await;
    }
}
