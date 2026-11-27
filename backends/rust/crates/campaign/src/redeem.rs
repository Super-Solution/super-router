//! Redeem codes: batches of serial codes (or one chosen code word) that turn into a grant, audited and limited so
//! the codes cannot be guessed.

use sqlx::Row;

use crate::codes::{self, generate_serial, SERIAL_BYTES};
use crate::error::{invalid, Error};
use crate::sink::{Cause, Grant, GrantKind};
use crate::store::{random, Campaigns};
use crate::types::*;

const MINUTE_MS: i64 = 60_000;
const DAY_MS: i64 = 86_400_000;

impl Campaigns {
    /// Makes a batch. The codes are returned **once**, in the clear; only keyed hashes and the last four symbols
    /// are stored. Everything happens in one transaction: a batch is whole or absent.
    pub async fn create_batch(&self, b: NewBatch<'_>) -> Result<CreatedBatch, Error> {
        let name = b.name.trim();
        if name.is_empty() || name.chars().count() > 80 {
            return invalid("a batch name is 1 to 80 characters");
        }
        if b.amount <= 0 || b.max_uses < 1 {
            return invalid("a code gives a positive amount and is usable at least once");
        }
        if b.grant_days.is_some_and(|d| !(1..=3650).contains(&d)) {
            return invalid("granted credits last 1 to 3650 days");
        }
        if b.redeem_until_ms.is_some_and(|t| t <= b.now_ms) {
            return invalid("the redeem-by time must be in the future");
        }
        let (kind, wanted): (&str, Vec<String>) = match b.custom {
            Some(word) => {
                let canonical =
                    codes::normalize_custom(word).ok_or_else(|| Error::Invalid("a code word is 4 to 32 letters and digits".into()))?;
                ("custom", vec![canonical])
            }
            None => {
                if !(1..=MAX_BATCH_CODES).contains(&b.count) {
                    return invalid(format!("a batch has 1 to {MAX_BATCH_CODES} codes"));
                }
                ("serial", Vec::new())
            }
        };
        let count = if kind == "custom" { 1 } else { b.count as i32 };
        let id = uuid::Uuid::new_v4().to_string();
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO sc_batches (id, name, kind, amount, max_uses, redeem_until, grant_days, code_count, created_by, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(&id)
        .bind(name)
        .bind(kind)
        .bind(b.amount)
        .bind(b.max_uses)
        .bind(b.redeem_until_ms)
        .bind(b.grant_days)
        .bind(count)
        .bind(b.created_by)
        .bind(b.now_ms)
        .execute(&mut *tx)
        .await?;
        let mut shown = Vec::with_capacity(count as usize);
        if kind == "custom" {
            let canonical = &wanted[0];
            let inserted = self.insert_code(&mut tx, &id, kind, canonical).await?;
            if !inserted {
                return invalid("that code word is already used by another batch");
            }
            shown.push(canonical.clone());
        } else {
            while shown.len() < count as usize {
                let canonical = generate_serial(&random::<SERIAL_BYTES>());
                if self.insert_code(&mut tx, &id, kind, &canonical).await? {
                    shown.push(codes::display_serial(&canonical));
                }
            }
        }
        tx.commit().await?;
        Ok(CreatedBatch { batch: id, codes: shown })
    }

    async fn insert_code(&self, tx: &mut sqlx::PgConnection, batch: &str, kind: &str, canonical: &str) -> Result<bool, Error> {
        let r = sqlx::query("INSERT INTO sc_codes (id, batch, hash, hint) VALUES ($1, $2, $3, $4) ON CONFLICT (hash) DO NOTHING")
            .bind(uuid::Uuid::new_v4().to_string())
            .bind(batch)
            .bind(codes::hash(&self.key, kind, canonical))
            .bind(codes::hint(canonical))
            .execute(tx)
            .await?;
        Ok(r.rows_affected() == 1)
    }

    /// A customer enters a code. Grants once per customer per code, within the batch's uses and dates.
    pub async fn redeem(&self, r: Redeem<'_>) -> Result<Result<Redeemed, RedeemError>, Error> {
        let s = self.settings().await?;
        let network =
            r.network.filter(|n| !n.is_empty()).map(|n| crate::signals::digest(&self.key, &crate::Signal::new("redeem-network", n)));
        let window_start = r.now_ms - i64::from(s.code_fail_window_minutes) * MINUTE_MS;
        let by_customer: i64 = sqlx::query_scalar("SELECT count(*) FROM sc_attempts WHERE customer = $1 AND at > $2")
            .bind(r.customer)
            .bind(window_start)
            .fetch_one(&self.pool)
            .await?;
        let by_network: i64 = match &network {
            Some(n) => {
                sqlx::query_scalar("SELECT count(*) FROM sc_attempts WHERE network = $1 AND at > $2")
                    .bind(n)
                    .bind(window_start)
                    .fetch_one(&self.pool)
                    .await?
            }
            None => 0,
        };
        let limit = i64::from(s.code_fail_limit);
        if by_customer >= limit || by_network >= limit * 3 {
            let oldest: Option<i64> =
                sqlx::query_scalar("SELECT min(at) FROM sc_attempts WHERE (customer = $1 OR network = $2) AND at > $3")
                    .bind(r.customer)
                    .bind(&network)
                    .bind(window_start)
                    .fetch_one(&self.pool)
                    .await?;
            let until_ms = oldest.unwrap_or(r.now_ms) + i64::from(s.code_fail_window_minutes) * MINUTE_MS;
            return Ok(Err(RedeemError::Locked { until_ms }));
        }

        let (serial, custom) = codes::candidates(r.code);
        let mut hashes = Vec::new();
        if let Some(c) = &serial {
            hashes.push(codes::hash(&self.key, "serial", c));
        }
        if let Some(c) = &custom {
            hashes.push(codes::hash(&self.key, "custom", c));
        }
        // A string that is neither shape cannot be any code and is not a guess at one.
        if hashes.is_empty() {
            return Ok(Err(RedeemError::Unknown));
        }

        let mut tx = self.pool.begin().await?;
        let row = sqlx::query(
            "SELECT c.id AS code, c.used, c.disabled_at AS code_disabled, c.hint,
                    b.id AS batch, b.amount, b.max_uses, b.redeem_until, b.grant_days, b.disabled_at AS batch_disabled
               FROM sc_codes c JOIN sc_batches b ON b.id = c.batch
              WHERE c.hash = ANY($1) LIMIT 1 FOR UPDATE OF c",
        )
        .bind(&hashes)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            tx.rollback().await?;
            // A syntactically valid serial that is not stored is a real guess; a custom-shaped string is a guess too.
            sqlx::query("INSERT INTO sc_attempts (customer, network, at) VALUES ($1, $2, $3)")
                .bind(r.customer)
                .bind(&network)
                .bind(r.now_ms)
                .execute(&self.pool)
                .await?;
            sqlx::query("DELETE FROM sc_attempts WHERE at < $1").bind(r.now_ms - DAY_MS).execute(&self.pool).await?;
            return Ok(Err(RedeemError::Unknown));
        };
        let code: String = row.get("code");
        let batch: String = row.get("batch");
        let amount: i64 = row.get("amount");
        let used: i32 = row.get("used");
        let max_uses: i32 = row.get("max_uses");
        let redeem_until: Option<i64> = row.get("redeem_until");
        let grant_days: Option<i32> = row.get("grant_days");
        let disabled = row.get::<Option<i64>, _>("code_disabled").is_some() || row.get::<Option<i64>, _>("batch_disabled").is_some();
        if disabled {
            return Ok(Err(RedeemError::Disabled));
        }
        if redeem_until.is_some_and(|t| r.now_ms > t) {
            return Ok(Err(RedeemError::Expired));
        }
        let mine: Option<i32> = sqlx::query_scalar("SELECT 1 FROM sc_redemptions WHERE code = $1 AND customer = $2")
            .bind(&code)
            .bind(r.customer)
            .fetch_optional(&mut *tx)
            .await?;
        if mine.is_some() {
            return Ok(Err(RedeemError::AlreadyRedeemed));
        }
        if used >= max_uses {
            return Ok(Err(RedeemError::Exhausted));
        }
        let key = format!("campaign:redeem:{code}:{}", r.customer);
        let expires_ms = grant_days.map(|d| r.now_ms + i64::from(d) * DAY_MS);
        sqlx::query(
            "INSERT INTO sc_redemptions (id, code, batch, customer, amount, grant_key, network, at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(&code)
        .bind(&batch)
        .bind(r.customer)
        .bind(amount)
        .bind(&key)
        .bind(&network)
        .bind(r.now_ms)
        .execute(&mut *tx)
        .await?;
        sqlx::query("UPDATE sc_codes SET used = used + 1 WHERE id = $1").bind(&code).execute(&mut *tx).await?;
        // Granted before the commit: a failure leaves nothing recorded, and a retry after a lost commit grants once
        // because the key is the same.
        self.sink
            .grant(&Grant {
                customer: r.customer.to_owned(),
                amount,
                kind: GrantKind::Promotion,
                expires_ms,
                key,
                cause: Cause::CodeRedeemed,
            })
            .await?;
        tx.commit().await?;
        Ok(Ok(Redeemed { amount, expires_ms, batch }))
    }

    /// Stops a batch (or, with `code`, one code of it) from being redeemed. Credits already given stay.
    pub async fn disable_batch(&self, batch: &str, now_ms: i64) -> Result<bool, Error> {
        let r = sqlx::query("UPDATE sc_batches SET disabled_at = $2 WHERE id = $1 AND disabled_at IS NULL")
            .bind(batch)
            .bind(now_ms)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected() > 0)
    }

    pub async fn disable_code(&self, code: &str, now_ms: i64) -> Result<bool, Error> {
        let r = sqlx::query("UPDATE sc_codes SET disabled_at = $2 WHERE id = $1 AND disabled_at IS NULL")
            .bind(code)
            .bind(now_ms)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected() > 0)
    }

    pub async fn batches(&self, limit: i64) -> Result<Vec<BatchRow>, Error> {
        Ok(sqlx::query_as::<_, BatchRow>(
            "SELECT b.id, b.name, b.kind, b.amount, b.max_uses, b.redeem_until, b.grant_days, b.code_count, b.created_by,
                    b.created_at, b.disabled_at,
                    (SELECT count(*) FROM sc_redemptions x WHERE x.batch = b.id) AS redeemed
               FROM sc_batches b ORDER BY b.created_at DESC, b.id LIMIT $1",
        )
        .bind(limit.clamp(1, 500))
        .fetch_all(&self.pool)
        .await?)
    }

    /// A batch's codes by their last four symbols and use, never the codes themselves.
    pub async fn codes(&self, batch: &str, limit: i64, offset: i64) -> Result<Vec<CodeRow>, Error> {
        Ok(sqlx::query_as::<_, CodeRow>(
            "SELECT id, hint, used, disabled_at FROM sc_codes WHERE batch = $1 ORDER BY used DESC, id LIMIT $2 OFFSET $3",
        )
        .bind(batch)
        .bind(limit.clamp(1, 1000))
        .bind(offset.max(0))
        .fetch_all(&self.pool)
        .await?)
    }

    /// Every redemption, newest first: who, which batch, which code (last four symbols), how much, when.
    pub async fn redemptions(&self, batch: Option<&str>, customer: Option<&str>, limit: i64) -> Result<Vec<RedemptionRow>, Error> {
        Ok(sqlx::query_as::<_, RedemptionRow>(
            "SELECT x.id, x.batch, b.name AS batch_name, c.hint AS code_hint, x.customer, x.amount, x.at
               FROM sc_redemptions x JOIN sc_batches b ON b.id = x.batch JOIN sc_codes c ON c.id = x.code
              WHERE ($1::text IS NULL OR x.batch = $1) AND ($2::text IS NULL OR x.customer = $2)
              ORDER BY x.at DESC, x.id LIMIT $3",
        )
        .bind(batch)
        .bind(customer)
        .bind(limit.clamp(1, 500))
        .fetch_all(&self.pool)
        .await?)
    }
}
