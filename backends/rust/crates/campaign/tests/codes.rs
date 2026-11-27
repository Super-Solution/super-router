mod common;

use std::sync::Arc;

use common::{open, DAY};
use super_router_campaign::*;

const T0: i64 = 1_800_000_000_000;

fn batch(name: &str, count: u32) -> NewBatch<'_> {
    NewBatch {
        name,
        amount: 500,
        count,
        custom: None,
        max_uses: 1,
        redeem_until_ms: None,
        grant_days: None,
        created_by: Some("admin-1"),
        now_ms: T0,
    }
}

async fn redeem(f: &common::Fixture, who: &str, code: &str, now: i64) -> Result<Redeemed, RedeemError> {
    f.c.redeem(Redeem { customer: who, code, network: Some("203.0.113.5"), now_ms: now }).await.unwrap()
}

#[tokio::test]
async fn a_batch_makes_distinct_codes_that_each_redeem_once() {
    let Some(f) = open().await else { return };
    let made = f.c.create_batch(batch("Launch cards", 50)).await.unwrap();
    assert_eq!(made.codes.len(), 50);
    let mut unique = made.codes.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), 50);
    assert!(made.codes.iter().all(|c| c.len() == 14 && c.matches('-').count() == 2));

    let first = redeem(&f, "alice", &made.codes[0], T0 + 1).await.unwrap();
    assert_eq!((first.amount, first.expires_ms), (500, None));
    assert_eq!(f.sink.total_for("alice"), 500);
    assert_eq!(redeem(&f, "alice", &made.codes[0], T0 + 2).await, Err(RedeemError::AlreadyRedeemed));
    assert_eq!(redeem(&f, "bob", &made.codes[0], T0 + 3).await, Err(RedeemError::Exhausted), "single use");
    assert_eq!(redeem(&f, "bob", &made.codes[1].to_lowercase().replace('-', " "), T0 + 4).await.unwrap().amount, 500, "typed loosely");
    assert_eq!(f.sink.count(), 2);

    let audit = f.c.redemptions(None, None, 10).await.unwrap();
    assert_eq!(audit.len(), 2);
    assert_eq!((audit[0].customer.as_str(), audit[0].batch_name.as_str(), audit[0].amount), ("bob", "Launch cards", 500));
    assert_eq!(audit[0].code_hint.len(), 4);
    assert_eq!(f.c.redemptions(None, Some("alice"), 10).await.unwrap().len(), 1);
    let rows = f.c.batches(10).await.unwrap();
    assert_eq!((rows[0].code_count, rows[0].redeemed), (50, 2));
    let codes = f.c.codes(&made.batch, 100, 0).await.unwrap();
    assert_eq!(codes.len(), 50);
    assert_eq!(codes.iter().filter(|c| c.used == 1).count(), 2);
}

#[tokio::test]
async fn codes_are_stored_only_as_hashes() {
    let Some(f) = open().await else { return };
    let made = f.c.create_batch(batch("Hidden", 5)).await.unwrap();
    let dump: Vec<(Vec<u8>, String)> = sqlx::query_as("SELECT hash, hint FROM sc_codes").fetch_all(f.c.pool()).await.unwrap();
    for code in &made.codes {
        let plain = code.replace('-', "");
        for (hash, hint) in &dump {
            assert_eq!(hash.len(), 32);
            assert!(!String::from_utf8_lossy(hash).contains(&plain));
            assert_eq!(hint.len(), 4);
        }
        assert!(dump.iter().any(|(_, h)| plain.ends_with(h.as_str())));
    }
    // A different secret cannot redeem them.
    let other = Campaigns::new(f.c.pool().clone(), f.sink.clone(), b"another-secret-another-secret-another!").unwrap();
    let out = other.redeem(Redeem { customer: "x", code: &made.codes[0], network: None, now_ms: T0 }).await.unwrap();
    assert_eq!(out, Err(RedeemError::Unknown));
}

#[tokio::test]
async fn dates_uses_and_switches_are_enforced() {
    let Some(f) = open().await else { return };
    let mut b = batch("Shared", 1);
    b.custom = Some("spring-2027");
    b.max_uses = 2;
    b.redeem_until_ms = Some(T0 + 10 * DAY);
    b.grant_days = Some(30);
    let made = f.c.create_batch(b).await.unwrap();
    assert_eq!(made.codes, vec!["SPRING2027".to_string()]);

    let a = redeem(&f, "alice", "Spring 2027", T0 + DAY).await.unwrap();
    assert_eq!(a.expires_ms, Some(T0 + DAY + 30 * DAY), "the credits last 30 days from redemption");
    redeem(&f, "bob", "SPRING2027", T0 + DAY).await.unwrap();
    assert_eq!(redeem(&f, "carol", "SPRING2027", T0 + DAY).await, Err(RedeemError::Exhausted), "two uses");
    assert!(f.sink.grants.lock().unwrap().values().next().unwrap().expires_ms.is_some());

    let mut late = batch("Dated", 1);
    late.redeem_until_ms = Some(T0 + DAY);
    let made = f.c.create_batch(late).await.unwrap();
    assert_eq!(redeem(&f, "alice", &made.codes[0], T0 + 2 * DAY).await, Err(RedeemError::Expired));
    assert!(f.c.create_batch(NewBatch { redeem_until_ms: Some(T0 - 1), ..batch("Past", 1) }).await.is_err());

    let made = f.c.create_batch(batch("Off", 2)).await.unwrap();
    assert!(f.c.disable_batch(&made.batch, T0).await.unwrap());
    assert!(!f.c.disable_batch(&made.batch, T0).await.unwrap());
    assert_eq!(redeem(&f, "alice", &made.codes[0], T0).await, Err(RedeemError::Disabled));
    let made = f.c.create_batch(batch("One off", 2)).await.unwrap();
    let rows = f.c.codes(&made.batch, 10, 0).await.unwrap();
    assert!(f.c.disable_code(&rows[0].id, T0).await.unwrap());
    let results = [redeem(&f, "dave", &made.codes[0], T0).await, redeem(&f, "dave", &made.codes[1], T0).await];
    assert_eq!(results.iter().filter(|r| matches!(r, Err(RedeemError::Disabled))).count(), 1);
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
}

#[tokio::test]
async fn a_word_cannot_be_used_by_two_batches_and_bad_batches_are_refused() {
    let Some(f) = open().await else { return };
    let mut b = batch("Word", 1);
    b.custom = Some("WELCOME");
    f.c.create_batch(NewBatch { ..b.clone() }).await.unwrap();
    assert!(f.c.create_batch(b.clone()).await.is_err());
    assert_eq!(f.c.batches(10).await.unwrap().len(), 1, "the refused batch left nothing behind");
    assert!(f.c.create_batch(NewBatch { custom: Some("a b"), ..b.clone() }).await.is_err());
    assert!(f.c.create_batch(batch("", 1)).await.is_err());
    assert!(f.c.create_batch(batch("Zero", 0)).await.is_err());
    assert!(f.c.create_batch(batch("Many", MAX_BATCH_CODES + 1)).await.is_err());
    assert!(f.c.create_batch(NewBatch { amount: 0, ..batch("Free", 1) }).await.is_err());
    assert!(f.c.create_batch(NewBatch { max_uses: 0, ..batch("Never", 1) }).await.is_err());
    assert!(f.c.create_batch(NewBatch { grant_days: Some(0), ..batch("Short", 1) }).await.is_err());
}

#[tokio::test]
async fn guessing_locks_the_customer_and_the_network_out_for_a_while() {
    let Some(f) = open().await else { return };
    let made = f.c.create_batch(batch("Real", 1)).await.unwrap();
    let limit = f.c.settings().await.unwrap().code_fail_limit as i64;
    for i in 0..limit {
        assert_eq!(redeem(&f, "mallory", &format!("GUESS{i:03}"), T0 + i).await, Err(RedeemError::Unknown));
    }
    // Even the real code is refused while locked.
    let locked = redeem(&f, "mallory", &made.codes[0], T0 + limit).await;
    let Err(RedeemError::Locked { until_ms }) = locked else { panic!("expected a lockout, got {locked:?}") };
    assert_eq!(until_ms, T0 + 60 * 60_000);
    // Another customer on a quiet network is not affected, and an hour later the lock is gone.
    assert!(f
        .c
        .redeem(Redeem { customer: "alice", code: &made.codes[0], network: Some("198.51.100.1"), now_ms: T0 + limit })
        .await
        .unwrap()
        .is_ok());
    let made2 = f.c.create_batch(batch("Real 2", 1)).await.unwrap();
    assert!(redeem(&f, "mallory", &made2.codes[0], T0 + 61 * 60_000).await.is_ok());
    // Things that cannot be a code at all are not counted.
    for i in 0..30 {
        assert_eq!(redeem(&f, "typist", "no!", T0 + 62 * 60_000 + i).await, Err(RedeemError::Unknown));
    }
    let made3 = f.c.create_batch(batch("Real 3", 1)).await.unwrap();
    assert!(f
        .c
        .redeem(Redeem { customer: "typist", code: &made3.codes[0], network: None, now_ms: T0 + 63 * 60_000 })
        .await
        .unwrap()
        .is_ok());
}

#[tokio::test]
async fn a_network_guessing_across_accounts_is_locked_too() {
    let Some(f) = open().await else { return };
    let limit = f.c.settings().await.unwrap().code_fail_limit as i64;
    let mut locked = false;
    for i in 0..(limit * 3 + 1) {
        let who = format!("sock{i}");
        let out =
            f.c.redeem(Redeem { customer: &who, code: &format!("GUESS{i:03}"), network: Some("192.0.2.77"), now_ms: T0 + i })
                .await
                .unwrap();
        locked = matches!(out, Err(RedeemError::Locked { .. }));
    }
    assert!(locked, "the network is locked after {} unknown codes across accounts", limit * 3);
}

#[tokio::test]
async fn a_failing_ledger_leaves_the_code_unused() {
    let Some(f) = open().await else { return };
    let made = f.c.create_batch(batch("Flaky", 1)).await.unwrap();
    f.sink.set_failing(true);
    assert!(matches!(
        f.c.redeem(Redeem { customer: "alice", code: &made.codes[0], network: None, now_ms: T0 }).await,
        Err(Error::Grant(_))
    ));
    assert!(f.c.redemptions(None, None, 10).await.unwrap().is_empty());
    assert_eq!(f.c.codes(&made.batch, 10, 0).await.unwrap()[0].used, 0);
    f.sink.set_failing(false);
    assert!(redeem(&f, "alice", &made.codes[0], T0 + 1).await.is_ok());
    assert_eq!(f.sink.count(), 1);
}

#[tokio::test]
async fn twenty_people_racing_for_one_single_use_code_get_exactly_one_grant() {
    let Some(f) = open().await else { return };
    let made = f.c.create_batch(batch("Race", 1)).await.unwrap();
    let c = Arc::new(f.c);
    let code = made.codes[0].clone();
    let tasks: Vec<_> = (0..20)
        .map(|i| {
            let c = c.clone();
            let code = code.clone();
            tokio::spawn(async move {
                let who = format!("racer{i}");
                c.redeem(Redeem { customer: &who, code: &code, network: None, now_ms: T0 }).await
            })
        })
        .collect();
    let mut wins = 0;
    for t in tasks {
        if t.await.unwrap().unwrap().is_ok() {
            wins += 1;
        }
    }
    assert_eq!(wins, 1);
    assert_eq!(f.sink.count(), 1);
    assert_eq!(c.codes(&made.batch, 10, 0).await.unwrap()[0].used, 1);
}
