//! The referral program: an inviter has a code, a new customer attaches it once, and the inviter earns a rate-class
//! share of what that customer buys, after a hold during which a dispute can void it.

use sqlx::{PgConnection, Row};

use crate::codes::{generate_invite, normalize_invite, INVITE_BYTES};
use crate::error::{invalid, Error};
use crate::rebate::rebate;
use crate::signals::digest;
use crate::sink::{Cause, Grant, GrantKind};
use crate::store::{random, Campaigns};
use crate::types::*;
use crate::Signal;

const DAY_MS: i64 = 86_400_000;
const HOUR_MS: i64 = 3_600_000;

/// What a periodic [`Campaigns::release_due`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Released {
    pub released: usize,
    /// Held rebates whose grant failed this time; they stay held and are tried again.
    pub failed: usize,
}

/// A person's decision about a flagged or active referral.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Review {
    /// A flagged referral counts from now on.
    Approve,
    /// The referral stops earning; held rebates are voided.
    Reject,
}

impl Campaigns {
    /// Remembers what the host reports about a customer, as keyed hashes.
    pub async fn record_signals(&self, customer: &str, signals: &[Signal], now_ms: i64) -> Result<(), Error> {
        let mut conn = self.pool.acquire().await?;
        self.put_signals(&mut conn, customer, signals, now_ms).await
    }

    async fn put_signals(&self, conn: &mut PgConnection, customer: &str, signals: &[Signal], now_ms: i64) -> Result<(), Error> {
        for s in signals {
            sqlx::query("INSERT INTO sc_signals (customer, signal, seen_at) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING")
                .bind(customer)
                .bind(digest(&self.key, s))
                .bind(now_ms)
                .execute(&mut *conn)
                .await?;
        }
        Ok(())
    }

    /// The customer's invite code, made on first use, with what it has earned. `signals` describe the customer now
    /// (used to catch the same person inviting themselves).
    pub async fn invite(&self, customer: &str, signals: &[Signal], now_ms: i64) -> Result<Invite, Error> {
        self.ensure_referrer(customer, now_ms).await?;
        self.record_signals(customer, signals, now_ms).await?;
        self.invite_view(customer).await?.ok_or_else(|| Error::Invalid("the invite code could not be made".into()))
    }

    async fn ensure_referrer(&self, customer: &str, now_ms: i64) -> Result<(), Error> {
        if customer.is_empty() || customer.len() > 100 {
            return invalid("a customer id is 1 to 100 characters");
        }
        for _ in 0..8 {
            let exists: Option<String> =
                sqlx::query_scalar("SELECT code FROM sc_referrers WHERE customer = $1").bind(customer).fetch_optional(&self.pool).await?;
            if exists.is_some() {
                return Ok(());
            }
            let code = generate_invite(&random::<INVITE_BYTES>());
            sqlx::query(
                "INSERT INTO sc_referrers (customer, code, class, created_at)
                 SELECT $1, $2, name, $3 FROM sc_rate_classes WHERE is_default
                 ON CONFLICT DO NOTHING",
            )
            .bind(customer)
            .bind(&code)
            .bind(now_ms)
            .execute(&self.pool)
            .await?;
        }
        invalid("an invite code could not be made")
    }

    /// The invite as its owner sees it; `None` when the customer has none yet.
    pub async fn invite_view(&self, customer: &str) -> Result<Option<Invite>, Error> {
        let row = sqlx::query(
            "SELECT r.code, r.class, c.rate_bps,
                    (SELECT count(*) FROM sc_referrals f WHERE f.referrer = r.customer AND f.state = 'active') AS invited,
                    COALESCE((SELECT sum(amount) FROM sc_rebates b WHERE b.referrer = r.customer AND b.state = 'released'), 0)::bigint AS earned,
                    COALESCE((SELECT sum(amount) FROM sc_rebates b WHERE b.referrer = r.customer AND b.state = 'held'), 0)::bigint AS pending
               FROM sc_referrers r JOIN sc_rate_classes c ON c.name = r.class WHERE r.customer = $1",
        )
        .bind(customer)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| Invite {
            code: r.get("code"),
            class: r.get("class"),
            rate_bps: r.get("rate_bps"),
            invited: r.get("invited"),
            earned: r.get("earned"),
            pending: r.get("pending"),
        }))
    }

    /// Puts a customer in a rate class (making their invite code when they have none). The class must exist.
    pub async fn set_class(&self, customer: &str, class: &str, now_ms: i64) -> Result<(), Error> {
        self.ensure_referrer(customer, now_ms).await?;
        let r = sqlx::query(
            "UPDATE sc_referrers SET class = $2 WHERE customer = $1 AND EXISTS (SELECT 1 FROM sc_rate_classes WHERE name = $2)",
        )
        .bind(customer)
        .bind(class)
        .execute(&self.pool)
        .await?;
        if r.rows_affected() == 0 {
            return invalid("there is no such rate class");
        }
        Ok(())
    }

    /// Attaches a new customer to the inviter who owns `code`, once and for good (the first one wins).
    pub async fn attach(&self, a: Attach<'_>) -> Result<AttachOutcome, Error> {
        let s = self.settings().await?;
        if !s.referral_enabled {
            return Ok(AttachOutcome::Disabled);
        }
        let Some(code) = normalize_invite(a.code) else { return Ok(AttachOutcome::UnknownCode) };
        if a.has_purchased || a.account_created_ms < a.now_ms - i64::from(s.attach_window_hours) * HOUR_MS {
            return Ok(AttachOutcome::TooLate);
        }
        let mut tx = self.pool.begin().await?;
        let referrer: Option<String> =
            sqlx::query_scalar("SELECT customer FROM sc_referrers WHERE code = $1").bind(&code).fetch_optional(&mut *tx).await?;
        let Some(referrer) = referrer else { return Ok(AttachOutcome::UnknownCode) };
        if referrer == a.referred {
            return Ok(AttachOutcome::SelfReferral);
        }
        let already: Option<i32> =
            sqlx::query_scalar("SELECT 1 FROM sc_referrals WHERE referred = $1").bind(a.referred).fetch_optional(&mut *tx).await?;
        if already.is_some() {
            return Ok(AttachOutcome::AlreadyAttached);
        }
        self.put_signals(&mut tx, a.referred, a.signals, a.now_ms).await?;
        let digests: Vec<Vec<u8>> = a.signals.iter().map(|s| digest(&self.key, s)).collect();
        let reason = self.suspicion(&mut tx, &referrer, a.referred, &digests, s.max_per_signal).await?;
        let state = if reason.is_some() { "flagged" } else { "active" };
        let inserted = sqlx::query(
            "INSERT INTO sc_referrals (referred, referrer, state, reason, attached_at) VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT DO NOTHING",
        )
        .bind(a.referred)
        .bind(&referrer)
        .bind(state)
        .bind(&reason)
        .bind(a.now_ms)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if inserted == 0 {
            return Ok(AttachOutcome::AlreadyAttached);
        }
        tx.commit().await?;
        Ok(match reason {
            Some(reason) => AttachOutcome::Flagged { reason },
            None => AttachOutcome::Attached,
        })
    }

    /// Why a new referral should earn nothing until a person looks at it, if it should.
    async fn suspicion(
        &self,
        conn: &mut PgConnection,
        referrer: &str,
        referred: &str,
        digests: &[Vec<u8>],
        max_per_signal: i32,
    ) -> Result<Option<String>, Error> {
        // The inviter attached to this very person: a pair vouching for each other.
        let cycle: Option<i32> = sqlx::query_scalar("SELECT 1 FROM sc_referrals WHERE referred = $1 AND referrer = $2")
            .bind(referrer)
            .bind(referred)
            .fetch_optional(&mut *conn)
            .await?;
        if cycle.is_some() {
            return Ok(Some("invited_each_other".into()));
        }
        if digests.is_empty() {
            return Ok(None);
        }
        let same: Option<i32> = sqlx::query_scalar("SELECT 1 FROM sc_signals WHERE customer = $1 AND signal = ANY($2) LIMIT 1")
            .bind(referrer)
            .bind(digests)
            .fetch_optional(&mut *conn)
            .await?;
        if same.is_some() {
            return Ok(Some("shares_a_signal_with_inviter".into()));
        }
        let crowd: i64 = sqlx::query_scalar(
            "SELECT count(DISTINCT f.referred) FROM sc_referrals f JOIN sc_signals g ON g.customer = f.referred
              WHERE f.referrer = $1 AND f.referred <> $2 AND g.signal = ANY($3)",
        )
        .bind(referrer)
        .bind(referred)
        .bind(digests)
        .fetch_one(&mut *conn)
        .await?;
        if crowd >= i64::from(max_per_signal) {
            return Ok(Some("many_invitees_share_a_signal".into()));
        }
        Ok(None)
    }

    /// A purchase by `referred` worth `base` (in the host's unit) with the host's own unique `reference` (a payment
    /// id): decides the inviter's rebate. Reporting the same reference twice decides it once.
    pub async fn purchase(&self, referred: &str, reference: &str, base: i64, now_ms: i64) -> Result<PurchaseOutcome, Error> {
        if reference.is_empty() || reference.len() > 255 {
            return invalid("a purchase reference is 1 to 255 characters");
        }
        let s = self.settings().await?;
        let mut tx = self.pool.begin().await?;
        let seen: Option<i32> =
            sqlx::query_scalar("SELECT 1 FROM sc_rebates WHERE reference = $1").bind(reference).fetch_optional(&mut *tx).await?;
        if seen.is_some() {
            return Ok(PurchaseOutcome::Seen);
        }
        if !s.referral_enabled || base <= 0 {
            return Ok(PurchaseOutcome::None);
        }
        let row = sqlx::query(
            "SELECT f.referrer, f.attached_at, f.state, c.rate_bps
               FROM sc_referrals f JOIN sc_referrers r ON r.customer = f.referrer JOIN sc_rate_classes c ON c.name = r.class
              WHERE f.referred = $1",
        )
        .bind(referred)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else { return Ok(PurchaseOutcome::None) };
        let referrer: String = row.get("referrer");
        let attached_at: i64 = row.get("attached_at");
        let state: String = row.get("state");
        let rate_bps: i32 = row.get("rate_bps");
        if state != "active" || s.rebate_window_days.is_some_and(|d| now_ms > attached_at + i64::from(d) * DAY_MS) {
            return Ok(PurchaseOutcome::None);
        }
        let mut amount = rebate(base, rate_bps);
        if let Some(cap) = s.rebate_cap {
            // One decision at a time per inviter, so two purchases cannot both slip under the cap.
            sqlx::query("SELECT 1 FROM sc_referrers WHERE customer = $1 FOR UPDATE").bind(&referrer).execute(&mut *tx).await?;
            let earned: i64 =
                sqlx::query_scalar("SELECT COALESCE(sum(amount), 0)::bigint FROM sc_rebates WHERE referrer = $1 AND state <> 'void'")
                    .bind(&referrer)
                    .fetch_one(&mut *tx)
                    .await?;
            amount = amount.min(cap.saturating_sub(earned));
        }
        if amount <= 0 {
            return Ok(PurchaseOutcome::None);
        }
        let held = s.hold_days > 0;
        let release_at = now_ms + i64::from(s.hold_days) * DAY_MS;
        let inserted = sqlx::query(
            "INSERT INTO sc_rebates (reference, referrer, referred, base, rate_bps, amount, state, release_at, created_at, settled_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) ON CONFLICT DO NOTHING",
        )
        .bind(reference)
        .bind(&referrer)
        .bind(referred)
        .bind(base)
        .bind(rate_bps)
        .bind(amount)
        .bind(if held { "held" } else { "released" })
        .bind(release_at)
        .bind(now_ms)
        .bind(if held { None } else { Some(now_ms) })
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if inserted == 0 {
            return Ok(PurchaseOutcome::Seen);
        }
        if !held {
            self.give(&referrer, amount, reference).await?;
        }
        tx.commit().await?;
        Ok(PurchaseOutcome::Rebate { amount, held })
    }

    async fn give(&self, referrer: &str, amount: i64, reference: &str) -> Result<(), Error> {
        self.sink
            .grant(&Grant {
                customer: referrer.to_owned(),
                amount,
                kind: GrantKind::Promotion,
                expires_ms: None,
                key: format!("campaign:rebate:{reference}"),
                cause: Cause::ReferralRebate,
            })
            .await?;
        Ok(())
    }

    /// Grants every held rebate whose time has come, at most `limit` per call. Call it from a periodic task; it is
    /// safe from several instances at once and safe to repeat.
    pub async fn release_due(&self, now_ms: i64, limit: i64) -> Result<Released, Error> {
        let due: Vec<String> = sqlx::query_scalar(
            "SELECT reference FROM sc_rebates WHERE state = 'held' AND release_at <= $1 ORDER BY release_at, reference LIMIT $2",
        )
        .bind(now_ms)
        .bind(limit.clamp(1, 1000))
        .fetch_all(&self.pool)
        .await?;
        let mut out = Released::default();
        for reference in due {
            match self.release_one(&reference, now_ms).await {
                Ok(true) => out.released += 1,
                Ok(false) => {}
                Err(_) => out.failed += 1,
            }
        }
        Ok(out)
    }

    async fn release_one(&self, reference: &str, now_ms: i64) -> Result<bool, Error> {
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query(
            "SELECT referrer, amount FROM sc_rebates WHERE reference = $1 AND state = 'held' AND release_at <= $2
             FOR UPDATE SKIP LOCKED",
        )
        .bind(reference)
        .bind(now_ms)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else { return Ok(false) };
        let referrer: String = row.get("referrer");
        let amount: i64 = row.get("amount");
        self.give(&referrer, amount, reference).await?;
        sqlx::query("UPDATE sc_rebates SET state = 'released', settled_at = $2 WHERE reference = $1")
            .bind(reference)
            .bind(now_ms)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(true)
    }

    /// Voids the held rebates earned on `referred`'s purchases (their payment was disputed). Granted ones stay:
    /// the router has no revoke, and the host decides what to do about them.
    pub async fn void_referred(&self, referred: &str, reason: &str, now_ms: i64) -> Result<u64, Error> {
        let r = sqlx::query("UPDATE sc_rebates SET state = 'void', settled_at = $2, reason = $3 WHERE referred = $1 AND state = 'held'")
            .bind(referred)
            .bind(now_ms)
            .bind(reason)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected())
    }

    /// A person's decision on a referral. Returns whether anything changed.
    pub async fn review(&self, referred: &str, decision: Review, now_ms: i64) -> Result<bool, Error> {
        let mut tx = self.pool.begin().await?;
        let changed = match decision {
            Review::Approve => {
                sqlx::query("UPDATE sc_referrals SET state = 'active', reviewed_at = $2 WHERE referred = $1 AND state = 'flagged'")
                    .bind(referred)
                    .bind(now_ms)
                    .execute(&mut *tx)
                    .await?
                    .rows_affected()
            }
            Review::Reject => {
                let n = sqlx::query("UPDATE sc_referrals SET state = 'void', reviewed_at = $2 WHERE referred = $1 AND state <> 'void'")
                    .bind(referred)
                    .bind(now_ms)
                    .execute(&mut *tx)
                    .await?
                    .rows_affected();
                sqlx::query(
                    "UPDATE sc_rebates SET state = 'void', settled_at = $2, reason = 'rejected' WHERE referred = $1 AND state = 'held'",
                )
                .bind(referred)
                .bind(now_ms)
                .execute(&mut *tx)
                .await?;
                n
            }
        };
        tx.commit().await?;
        Ok(changed > 0)
    }

    pub async fn referrers(&self, class: Option<&str>, limit: i64, offset: i64) -> Result<Vec<ReferrerRow>, Error> {
        Ok(sqlx::query_as::<_, ReferrerRow>(
            "SELECT r.customer, r.code, r.class, r.created_at,
                    (SELECT count(*) FROM sc_referrals f WHERE f.referrer = r.customer AND f.state = 'active') AS invited,
                    (SELECT count(*) FROM sc_referrals f WHERE f.referrer = r.customer AND f.state = 'flagged') AS flagged,
                    COALESCE((SELECT sum(amount) FROM sc_rebates b WHERE b.referrer = r.customer AND b.state = 'released'), 0)::bigint AS earned,
                    COALESCE((SELECT sum(amount) FROM sc_rebates b WHERE b.referrer = r.customer AND b.state = 'held'), 0)::bigint AS pending
               FROM sc_referrers r
              WHERE ($1::text IS NULL OR r.class = $1)
              ORDER BY earned DESC, invited DESC, r.created_at, r.customer LIMIT $2 OFFSET $3",
        )
        .bind(class)
        .bind(limit.clamp(1, 500))
        .bind(offset.max(0))
        .fetch_all(&self.pool)
        .await?)
    }

    /// Referrals newest first; `state` and `referrer` narrow them.
    pub async fn referrals(&self, state: Option<&str>, referrer: Option<&str>, limit: i64) -> Result<Vec<ReferralRow>, Error> {
        Ok(sqlx::query_as::<_, ReferralRow>(
            "SELECT referred, referrer, state, reason, attached_at FROM sc_referrals
              WHERE ($1::text IS NULL OR state = $1) AND ($2::text IS NULL OR referrer = $2)
              ORDER BY attached_at DESC, referred LIMIT $3",
        )
        .bind(state)
        .bind(referrer)
        .bind(limit.clamp(1, 500))
        .fetch_all(&self.pool)
        .await?)
    }

    /// Rebates newest first; `state` narrows them.
    pub async fn rebates(&self, state: Option<&str>, limit: i64) -> Result<Vec<RebateRow>, Error> {
        Ok(sqlx::query_as::<_, RebateRow>(
            "SELECT reference, referrer, referred, base, rate_bps, amount, state, release_at, created_at, settled_at, reason
               FROM sc_rebates WHERE ($1::text IS NULL OR state = $1) ORDER BY created_at DESC, reference LIMIT $2",
        )
        .bind(state)
        .bind(limit.clamp(1, 500))
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn totals(&self) -> Result<Totals, Error> {
        let r = sqlx::query(
            "SELECT (SELECT count(*) FROM sc_referrers) AS referrers,
                    (SELECT count(*) FROM sc_referrals WHERE state = 'active') AS referrals,
                    (SELECT count(*) FROM sc_referrals WHERE state = 'flagged') AS flagged,
                    COALESCE((SELECT sum(amount) FROM sc_rebates WHERE state = 'held'), 0)::bigint AS rebates_held,
                    COALESCE((SELECT sum(amount) FROM sc_rebates WHERE state = 'released'), 0)::bigint AS rebates_released,
                    COALESCE((SELECT sum(amount) FROM sc_rebates WHERE state = 'void'), 0)::bigint AS rebates_void,
                    (SELECT count(*) FROM sc_codes) AS codes,
                    (SELECT count(*) FROM sc_redemptions) AS redemptions,
                    COALESCE((SELECT sum(amount) FROM sc_redemptions), 0)::bigint AS redeemed_amount",
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(Totals {
            referrers: r.get("referrers"),
            referrals: r.get("referrals"),
            flagged: r.get("flagged"),
            rebates_held: r.get("rebates_held"),
            rebates_released: r.get("rebates_released"),
            rebates_void: r.get("rebates_void"),
            codes: r.get("codes"),
            redemptions: r.get("redemptions"),
            redeemed_amount: r.get("redeemed_amount"),
        })
    }
}
