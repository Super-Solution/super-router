//! Shared setup: a schema of its own per test on `CAMPAIGN_TEST_DATABASE_URL` (or `DATABASE_URL`, or the local
//! development database), skipped when it is unreachable unless `SR_REQUIRE_POSTGRES` is set, as CI should.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use sqlx::postgres::PgPoolOptions;
use sqlx::{Executor, PgPool};
use super_router_campaign::{Campaigns, Grant, GrantSink, SinkError};

pub const LOCAL_URL: &str = "postgresql://superrouter:local-development-only@127.0.0.1:25432/superrouter";
pub const SECRET: &[u8] = b"0123456789abcdef0123456789abcdef-test-secret";
pub const DAY: i64 = 86_400_000;

/// A sink that remembers what it was told, idempotent on the key like a real one, and can be made to fail.
#[derive(Default)]
pub struct Recorder {
    pub grants: Mutex<HashMap<String, Grant>>,
    pub fail: Mutex<bool>,
}

impl Recorder {
    pub fn count(&self) -> usize {
        self.grants.lock().unwrap().len()
    }
    pub fn total_for(&self, customer: &str) -> i64 {
        self.grants.lock().unwrap().values().filter(|g| g.customer == customer).map(|g| g.amount).sum()
    }
    pub fn set_failing(&self, on: bool) {
        *self.fail.lock().unwrap() = on;
    }
}

#[async_trait]
impl GrantSink for Recorder {
    async fn grant(&self, g: &Grant) -> Result<(), SinkError> {
        if *self.fail.lock().unwrap() {
            return Err(SinkError("the ledger is down".into()));
        }
        let mut all = self.grants.lock().unwrap();
        match all.get(&g.key) {
            Some(old) if old.amount == g.amount => Ok(()),
            Some(_) => Err(SinkError("idempotency_conflict".into())),
            None => {
                all.insert(g.key.clone(), g.clone());
                Ok(())
            }
        }
    }
}

pub struct Fixture {
    pub c: Campaigns,
    pub sink: Arc<Recorder>,
}

pub fn database_url() -> String {
    std::env::var("CAMPAIGN_TEST_DATABASE_URL")
        .ok()
        .or_else(|| std::env::var("DATABASE_URL").ok())
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| LOCAL_URL.to_string())
}

/// A migrated, empty campaign schema; `None` (after saying why) when the database does not answer and is not required.
pub async fn open() -> Option<Fixture> {
    let required = std::env::var("SR_REQUIRE_POSTGRES").is_ok_and(|v| !v.is_empty());
    let schema = format!("sc_test_{}", uuid::Uuid::new_v4().simple());
    let url = database_url();
    let admin = tokio::time::timeout(Duration::from_secs(3), PgPoolOptions::new().max_connections(2).connect(&url)).await;
    let admin: PgPool = match admin {
        Ok(Ok(p)) => p,
        other => {
            let why = match other {
                Ok(Err(e)) => e.to_string(),
                _ => "timed out".to_string(),
            };
            assert!(!required, "postgres required but unreachable: {why}");
            eprintln!("skipping: postgres unreachable (set CAMPAIGN_TEST_DATABASE_URL): {why}");
            return None;
        }
    };
    admin.execute(format!("CREATE SCHEMA {schema}").as_str()).await.unwrap();
    let path = schema.clone();
    let pool = PgPoolOptions::new()
        .max_connections(24)
        .after_connect(move |conn, _| {
            let path = path.clone();
            Box::pin(async move {
                conn.execute(format!("SET search_path TO {path}").as_str()).await?;
                Ok(())
            })
        })
        .connect(&url)
        .await
        .unwrap();
    Campaigns::migrate(&pool).await.unwrap();
    let sink = Arc::new(Recorder::default());
    let c = Campaigns::new(pool, sink.clone(), SECRET).unwrap();
    Some(Fixture { c, sink })
}
