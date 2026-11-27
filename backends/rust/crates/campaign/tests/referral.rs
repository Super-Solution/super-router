mod common;

use common::{open, DAY};
use super_router_campaign::*;

const T0: i64 = 1_800_000_000_000;

async fn invited(f: &common::Fixture, inviter: &str, who: &str, signals: &[Signal]) -> AttachOutcome {
    let code = f.c.invite(inviter, &[], T0).await.unwrap().code;
    f.c.attach(Attach { referred: who, code: &code, account_created_ms: T0, has_purchased: false, signals, now_ms: T0 }).await.unwrap()
}

#[tokio::test]
async fn defaults_are_three_percent_and_a_week_of_hold() {
    let Some(f) = open().await else { return };
    let s = f.c.settings().await.unwrap();
    assert!(s.referral_enabled);
    assert_eq!(s.hold_days, 7);
    let classes = f.c.rate_classes().await.unwrap();
    assert_eq!(classes.len(), 1);
    assert_eq!((classes[0].name.as_str(), classes[0].rate_bps, classes[0].is_default), ("default", 300, true));
    Campaigns::migrate(f.c.pool()).await.unwrap();
    Campaigns::check_applied(f.c.pool()).await.unwrap();
}

#[tokio::test]
async fn an_inviter_earns_the_default_rate_after_the_hold_and_only_once_per_purchase() {
    let Some(f) = open().await else { return };
    assert_eq!(invited(&f, "alice", "bob", &[]).await, AttachOutcome::Attached);

    let bought = f.c.purchase("bob", "cs_1", 1000, T0 + DAY).await.unwrap();
    assert_eq!(bought, PurchaseOutcome::Rebate { amount: 30, held: true });
    assert_eq!(f.c.purchase("bob", "cs_1", 1000, T0 + DAY).await.unwrap(), PurchaseOutcome::Seen);
    assert_eq!(f.sink.count(), 0, "held, nothing granted yet");

    let early = f.c.release_due(T0 + 7 * DAY, 100).await.unwrap();
    assert_eq!(early.released, 0, "the hold runs from the purchase, not the attachment");
    let on_time = f.c.release_due(T0 + 8 * DAY, 100).await.unwrap();
    assert_eq!(on_time.released, 1);
    assert_eq!(f.sink.total_for("alice"), 30);
    assert_eq!(f.c.release_due(T0 + 9 * DAY, 100).await.unwrap().released, 0, "released once");

    let invite = f.c.invite_view("alice").await.unwrap().unwrap();
    assert_eq!((invite.invited, invite.earned, invite.pending, invite.rate_bps), (1, 30, 0, 300));
    let key = f.sink.grants.lock().unwrap().keys().next().unwrap().clone();
    assert_eq!(key, "campaign:rebate:cs_1");
}

#[tokio::test]
async fn a_partner_class_pays_more_and_a_rate_change_does_not_touch_decided_rebates() {
    let Some(f) = open().await else { return };
    f.c.ensure_rate_class("kol", 500, T0).await.unwrap();
    f.c.ensure_rate_class("kol", 900, T0).await.unwrap();
    assert_eq!(f.c.rate_classes().await.unwrap().iter().find(|c| c.name == "kol").unwrap().rate_bps, 500, "ensure keeps an existing rate");
    f.c.set_class("alice", "kol", T0).await.unwrap();
    assert_eq!(invited(&f, "alice", "bob", &[]).await, AttachOutcome::Attached);
    assert_eq!(f.c.purchase("bob", "cs_1", 2000, T0).await.unwrap(), PurchaseOutcome::Rebate { amount: 100, held: true });
    f.c.set_rate("kol", 1000, T0 + 1).await.unwrap();
    assert_eq!(f.c.purchase("bob", "cs_2", 2000, T0 + 2).await.unwrap(), PurchaseOutcome::Rebate { amount: 200, held: true });
    f.c.release_due(T0 + 30 * DAY, 100).await.unwrap();
    assert_eq!(f.sink.total_for("alice"), 300);
    assert!(f.c.set_class("alice", "nope", T0).await.is_err());
    assert!(f.c.set_rate("Bad Name", 100, T0).await.is_err());
    assert!(f.c.set_rate("fine", 10_001, T0).await.is_err());
    assert!(!f.c.remove_rate_class("default").await.unwrap(), "the default stays");
    assert!(!f.c.remove_rate_class("kol").await.unwrap(), "a class with members stays");
}

#[tokio::test]
async fn nobody_can_claim_an_existing_customer_or_attach_twice_or_to_themselves() {
    let Some(f) = open().await else { return };
    let code = f.c.invite("alice", &[], T0).await.unwrap().code;
    let a = |who: &'static str, created: i64, bought: bool, code: &str, now: i64| {
        let code = code.to_owned();
        let c = &f.c;
        async move {
            c.attach(Attach { referred: who, code: &code, account_created_ms: created, has_purchased: bought, signals: &[], now_ms: now })
                .await
                .unwrap()
        }
    };
    assert_eq!(a("alice", T0, false, &code, T0).await, AttachOutcome::SelfReferral);
    assert_eq!(a("old", T0 - 100 * DAY, false, &code, T0).await, AttachOutcome::TooLate, "account older than the window");
    assert_eq!(a("buyer", T0, true, &code, T0).await, AttachOutcome::TooLate, "already bought");
    assert_eq!(a("x", T0, false, "ZZZZZZZZ", T0).await, AttachOutcome::UnknownCode);
    assert_eq!(a("x", T0, false, "short", T0).await, AttachOutcome::UnknownCode);
    assert_eq!(a("bob", T0, false, &code, T0).await, AttachOutcome::Attached);
    assert_eq!(a("bob", T0, false, &code, T0).await, AttachOutcome::AlreadyAttached);
    let other = f.c.invite("carol", &[], T0).await.unwrap().code;
    assert_eq!(a("bob", T0, false, &other, T0).await, AttachOutcome::AlreadyAttached, "first touch wins");
    // A lower-case, hyphenated code works as typed.
    let typed = format!("{}-{}", code[..4].to_lowercase(), code[4..].to_lowercase());
    assert_eq!(a("dave", T0, false, &typed, T0).await, AttachOutcome::Attached);
}

#[tokio::test]
async fn look_alike_accounts_earn_nothing_until_a_person_clears_them() {
    let Some(f) = open().await else { return };
    let net = Signal::network("203.0.113.9").unwrap();
    f.c.invite("alice", &[net.clone(), Signal::email("Alice.A+x@gmail.com").unwrap()], T0).await.unwrap();

    // The same person under another address.
    let same_mail = Signal::email("aliceA@gmail.com").unwrap();
    let outcome = invited(&f, "alice", "alt", &[same_mail]).await;
    assert_eq!(outcome, AttachOutcome::Flagged { reason: "shares_a_signal_with_inviter".into() });
    assert_eq!(f.c.purchase("alt", "cs_alt", 1000, T0).await.unwrap(), PurchaseOutcome::None);
    assert!(f.c.review("alt", Review::Approve, T0).await.unwrap());
    assert_eq!(f.c.purchase("alt", "cs_alt2", 1000, T0).await.unwrap(), PurchaseOutcome::Rebate { amount: 30, held: true });

    // A crowd on one network: the fourth is flagged (three are allowed by default).
    let crowd = Signal::network("198.51.100.4").unwrap();
    for who in ["c1", "c2", "c3"] {
        assert_eq!(invited(&f, "alice", who, std::slice::from_ref(&crowd)).await, AttachOutcome::Attached, "{who}");
    }
    assert_eq!(
        invited(&f, "alice", "c4", std::slice::from_ref(&crowd)).await,
        AttachOutcome::Flagged { reason: "many_invitees_share_a_signal".into() }
    );

    // Two people vouching for each other.
    let code_bob = f.c.invite("bob", &[], T0).await.unwrap().code;
    f.c.attach(Attach { referred: "carol", code: &code_bob, account_created_ms: T0, has_purchased: false, signals: &[], now_ms: T0 })
        .await
        .unwrap();
    let code_carol = f.c.invite("carol", &[], T0).await.unwrap().code;
    let back =
        f.c.attach(Attach { referred: "bob", code: &code_carol, account_created_ms: T0, has_purchased: false, signals: &[], now_ms: T0 })
            .await
            .unwrap();
    assert_eq!(back, AttachOutcome::Flagged { reason: "invited_each_other".into() });

    let flagged = f.c.referrals(Some("flagged"), None, 50).await.unwrap();
    assert_eq!(flagged.len(), 2);
    assert!(f.c.review("c4", Review::Reject, T0).await.unwrap());
    assert_eq!(f.c.referrals(Some("flagged"), None, 50).await.unwrap().len(), 1);
    assert!(!f.c.review("c4", Review::Approve, T0).await.unwrap(), "a rejected referral stays rejected");
}

#[tokio::test]
async fn signals_are_stored_as_hashes_only() {
    let Some(f) = open().await else { return };
    f.c.invite("alice", &[Signal::email("secret.person@example.org").unwrap(), Signal::network("203.0.113.9").unwrap()], T0).await.unwrap();
    let stored: Vec<(Vec<u8>,)> = sqlx::query_as("SELECT signal FROM sc_signals").fetch_all(f.c.pool()).await.unwrap();
    assert_eq!(stored.len(), 2);
    for (s,) in stored {
        assert_eq!(s.len(), 32);
        let text = String::from_utf8_lossy(&s).to_string();
        assert!(!text.contains("secret") && !text.contains("203.0.113"));
    }
}

#[tokio::test]
async fn a_disputed_purchase_voids_its_held_rebate_and_nothing_is_granted() {
    let Some(f) = open().await else { return };
    assert_eq!(invited(&f, "alice", "bob", &[]).await, AttachOutcome::Attached);
    f.c.purchase("bob", "cs_1", 1000, T0).await.unwrap();
    f.c.purchase("bob", "cs_2", 3000, T0).await.unwrap();
    assert_eq!(f.c.void_referred("bob", "dispute", T0 + DAY).await.unwrap(), 2);
    assert_eq!(f.c.release_due(T0 + 30 * DAY, 100).await.unwrap().released, 0);
    assert_eq!(f.sink.count(), 0);
    let rebates = f.c.rebates(Some("void"), 10).await.unwrap();
    assert_eq!(rebates.len(), 2);
    assert_eq!(rebates[0].reason.as_deref(), Some("dispute"));
    // A rebate already released is not taken back.
    f.c.purchase("bob", "cs_3", 1000, T0 + 40 * DAY).await.unwrap();
    f.c.release_due(T0 + 50 * DAY, 100).await.unwrap();
    assert_eq!(f.c.void_referred("bob", "dispute", T0 + 51 * DAY).await.unwrap(), 0);
    assert_eq!(f.sink.total_for("alice"), 30);
}

#[tokio::test]
async fn caps_windows_and_the_switch_limit_what_an_inviter_can_earn() {
    let Some(f) = open().await else { return };
    let mut s = f.c.settings().await.unwrap();
    s.hold_days = 0;
    s.rebate_cap = Some(50);
    s.rebate_window_days = Some(10);
    f.c.set_settings(&s, T0).await.unwrap();
    assert_eq!(invited(&f, "alice", "bob", &[]).await, AttachOutcome::Attached);
    assert_eq!(f.c.purchase("bob", "cs_1", 1000, T0).await.unwrap(), PurchaseOutcome::Rebate { amount: 30, held: false });
    assert_eq!(f.sink.total_for("alice"), 30, "no hold: granted at once");
    assert_eq!(
        f.c.purchase("bob", "cs_2", 1000, T0).await.unwrap(),
        PurchaseOutcome::Rebate { amount: 20, held: false },
        "clamped to the cap"
    );
    assert_eq!(f.c.purchase("bob", "cs_3", 1000, T0).await.unwrap(), PurchaseOutcome::None, "cap reached");
    assert_eq!(f.sink.total_for("alice"), 50);

    s.rebate_cap = None;
    f.c.set_settings(&s, T0).await.unwrap();
    assert_eq!(f.c.purchase("bob", "cs_4", 1000, T0 + 11 * DAY).await.unwrap(), PurchaseOutcome::None, "outside the window");
    assert_eq!(f.c.purchase("bob", "cs_5", 1000, T0 + 9 * DAY).await.unwrap(), PurchaseOutcome::Rebate { amount: 30, held: false });

    s.referral_enabled = false;
    f.c.set_settings(&s, T0).await.unwrap();
    assert_eq!(f.c.purchase("bob", "cs_6", 1000, T0).await.unwrap(), PurchaseOutcome::None);
    assert_eq!(invited(&f, "alice", "zed", &[]).await, AttachOutcome::Disabled);

    s.hold_days = 91;
    assert!(f.c.set_settings(&s, T0).await.is_err());
}

#[tokio::test]
async fn a_failing_ledger_records_nothing_and_a_retry_grants_once() {
    let Some(f) = open().await else { return };
    let mut s = f.c.settings().await.unwrap();
    s.hold_days = 0;
    f.c.set_settings(&s, T0).await.unwrap();
    assert_eq!(invited(&f, "alice", "bob", &[]).await, AttachOutcome::Attached);
    f.sink.set_failing(true);
    assert!(matches!(f.c.purchase("bob", "cs_1", 1000, T0).await, Err(Error::Grant(_))));
    assert!(f.c.rebates(None, 10).await.unwrap().is_empty(), "the decision was not kept");
    f.sink.set_failing(false);
    assert_eq!(f.c.purchase("bob", "cs_1", 1000, T0).await.unwrap(), PurchaseOutcome::Rebate { amount: 30, held: false });
    assert_eq!(f.sink.count(), 1);

    // Held rebates: a failing release is counted and retried, never lost.
    s.hold_days = 1;
    f.c.set_settings(&s, T0).await.unwrap();
    f.c.purchase("bob", "cs_2", 1000, T0).await.unwrap();
    f.sink.set_failing(true);
    let r = f.c.release_due(T0 + 2 * DAY, 10).await.unwrap();
    assert_eq!((r.released, r.failed), (0, 1));
    f.sink.set_failing(false);
    assert_eq!(f.c.release_due(T0 + 2 * DAY, 10).await.unwrap().released, 1);
    assert_eq!(f.sink.total_for("alice"), 60);
}

#[tokio::test]
async fn overview_numbers_add_up() {
    let Some(f) = open().await else { return };
    assert_eq!(invited(&f, "alice", "bob", &[]).await, AttachOutcome::Attached);
    f.c.purchase("bob", "cs_1", 1000, T0).await.unwrap();
    let t = f.c.totals().await.unwrap();
    assert_eq!((t.referrers, t.referrals, t.rebates_held, t.rebates_released), (1, 1, 30, 0));
    let rows = f.c.referrers(None, 10, 0).await.unwrap();
    assert_eq!((rows[0].customer.as_str(), rows[0].invited, rows[0].pending), ("alice", 1, 30));
    assert_eq!(f.c.referrers(Some("nope"), 10, 0).await.unwrap().len(), 0);
}
