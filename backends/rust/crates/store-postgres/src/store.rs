//! The PostgreSQL implementation of [`DataStore`].
//!
//! The schema is shared with the TypeScript and Go stores, so all three can serve the same database. Every write
//! runs in one transaction that first takes an advisory lock on the tenant (workspace and environment), which
//! serialises admission, holds and settlement per tenant. Row locks (`FOR UPDATE`) are taken in a fixed order so
//! the settlement of one request never deadlocks with the admission of another.

use std::sync::Arc;

use async_trait::async_trait;
use include_dir::{include_dir, Dir};
use num_bigint::BigInt;
use num_traits::{Signed, Zero};
use serde_json::json;
use sqlx::postgres::{PgPool, PgPoolOptions, PgRow};
use sqlx::{Executor, PgConnection, Postgres, Row, Transaction};
use super_router_contracts::{parse_service, Meters, Offering, Principal, Service};
use super_router_pricing::charge;
use super_router_router::{system_clock, BalanceRow, Clock, DataStore, Decision, Leg, RequestRecord, Reservation, Store, StoreResult};
use uuid::Uuid;

use crate::util::{array_arg, array_expr, atoms, db, fail, internal, iso_millis, json_strings, json_text, ms, now_ms, opt_text, text, ts};
use crate::windows::{admit, settle_windows, window_error, window_specs, WindowBlock};

/// This backend's own migrations (`crates/store-postgres/migrations`): the Rust store's schema, versioned apart from the
/// TypeScript, Go and Python stores.
static MIGRATIONS: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/migrations");

/// The embedded migration files as `(file name, bytes)`, in name order.
pub fn embedded_migrations() -> Vec<(&'static str, &'static [u8])> {
    let mut files: Vec<_> = MIGRATIONS
        .files()
        .filter_map(|f| Some((f.path().file_name()?.to_str()?, f.contents())))
        .filter(|(name, _)| name.ends_with(".sql"))
        .collect();
    files.sort_by_key(|(name, _)| *name);
    files
}

/// Re-checks, inside each write transaction and before it changes anything, that the principal's credential is
/// still good. Hosts that authenticate with database-backed keys pass [`crate::KeyRevalidator`].
#[async_trait]
pub trait Revalidate: Send + Sync {
    /// `scope` is what the operation needs: `service:write`, `billing:write` or `invoke`.
    async fn revalidate(&self, conn: &mut PgConnection, p: &Principal, scope: &str) -> StoreResult<()>;
}

/// Configures a [`PostgresStore`].
#[derive(Default, Clone)]
pub struct Options {
    /// Used for the dispatch deadline check. `None` means the wall clock. SQL expiry always uses the database clock.
    pub clock: Option<Clock>,
    pub revalidate: Option<Arc<dyn Revalidate>>,
}

/// The optional parts of a grant.
#[derive(Debug, Clone, Default)]
pub struct GrantOptions {
    /// Epoch milliseconds.
    pub expires: Option<i64>,
    /// `None` means every service.
    pub service_ids: Option<Vec<String>>,
    /// `purchased` (the default), `promotion` or `subscription`.
    pub source_kind: String,
}

/// A funding bucket as reported by [`PostgresStore::buckets`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BucketRow {
    pub source: String,
    pub remaining: String,
    pub held: String,
}

/// [`DataStore`] on PostgreSQL.
#[derive(Clone)]
pub struct PostgresStore {
    pool: PgPool,
    clock: Clock,
    opts: Options,
}

const REQUEST_COLUMNS: &str = "id,customer,digest,service::text AS service,legs::text AS legs,execution,billing,cancel_requested,output_started,provider_spent::text AS provider_spent";

fn request_columns() -> String {
    format!("{REQUEST_COLUMNS},{} AS deadline_ms", ms("deadline"))
}

fn valid_amount(s: &str) -> bool {
    (1..=30).contains(&s.len()) && !s.starts_with('0') && s.bytes().all(|c| c.is_ascii_digit())
}

fn scan_request(workspace: &str, environment: &str, row: &PgRow) -> StoreResult<(RequestRecord, String)> {
    let id = text(row, "id")?;
    let service =
        parse_service(text(row, "service")?.as_bytes()).map_err(|e| internal(format!("stored service of request {id} is invalid: {e}")))?;
    let legs: Vec<Leg> = serde_json::from_str(&text(row, "legs")?).map_err(|e| internal(format!("stored legs of request {id}: {e}")))?;
    let record = RequestRecord {
        id,
        workspace: workspace.to_string(),
        environment: environment.to_string(),
        customer: text(row, "customer")?,
        service,
        execution: text(row, "execution")?,
        billing: text(row, "billing")?,
        cancel_requested: row.try_get("cancel_requested").map_err(db)?,
        output_started: row.try_get("output_started").map_err(db)?,
        deadline: row.try_get("deadline_ms").map_err(db)?,
        legs,
        provider_spent: text(row, "provider_spent")?,
    };
    Ok((record, text(row, "digest")?))
}

fn parse_stored(raw: &str) -> StoreResult<Service> {
    parse_service(raw.as_bytes()).map_err(|e| internal(format!("stored service is invalid: {e}")))
}

/// Commits when the work succeeded, rolls back (and surfaces the failure) when it did not.
async fn conclude<T>(tx: Transaction<'static, Postgres>, result: StoreResult<T>) -> StoreResult<T> {
    match result {
        Ok(v) => {
            tx.commit().await.map_err(db)?;
            Ok(v)
        }
        Err(e) => {
            let _ = tx.rollback().await;
            Err(e)
        }
    }
}

impl PostgresStore {
    /// Creates a store on an open pool. The caller owns the pool.
    pub fn new(pool: PgPool, opts: Options) -> Self {
        let clock = opts.clock.clone().unwrap_or_else(system_clock);
        Self { pool, clock, opts }
    }

    /// Opens a pool on `url` and wraps it.
    pub async fn connect(url: &str, max_connections: u32, opts: Options) -> Result<Self, sqlx::Error> {
        let pool =
            PgPoolOptions::new().max_connections(max_connections).acquire_timeout(std::time::Duration::from_secs(30)).connect(url).await?;
        Ok(Self::new(pool, opts))
    }

    /// The pool, for hosts that add their own tables or queries beside the router's.
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Applies the embedded migrations that have not run yet, under an advisory lock so several instances can start
    /// at once. Bookkeeping is the `sr_migrations(version)` table. The other backends keep a different migration
    /// history, so give this one its own database or schema. A database that is ahead of this build (a newer schema
    /// than any embedded file) is refused.
    pub async fn migrate(&self) -> StoreResult<()> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        let result = Self::migrate_tx(&mut tx).await;
        conclude(tx, result).await
    }

    async fn migrate_tx(conn: &mut PgConnection) -> StoreResult<()> {
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('superrouter:migrate',0))").execute(&mut *conn).await.map_err(db)?;
        sqlx::query("CREATE TABLE IF NOT EXISTS sr_migrations(version integer PRIMARY KEY)").execute(&mut *conn).await.map_err(db)?;
        let mut files = Vec::new();
        for (name, body) in embedded_migrations() {
            let version: i32 = name
                .get(..3)
                .and_then(|v| v.parse().ok())
                .filter(|_| name.as_bytes().get(3) == Some(&b'_'))
                .ok_or_else(|| internal(format!("migration {name:?} has no version prefix")))?;
            files.push((version, name, body));
        }
        let latest = files.iter().map(|f| f.0).max().unwrap_or(0);
        let applied: Vec<i32> = sqlx::query("SELECT version FROM sr_migrations")
            .fetch_all(&mut *conn)
            .await
            .map_err(db)?
            .iter()
            .map(|r| r.try_get("version"))
            .collect::<Result<_, _>>()
            .map_err(db)?;
        if let Some(ahead) = applied.iter().find(|v| **v > latest) {
            return Err(internal(format!("the database schema is ahead of this build: migration {ahead} is newer than {latest}")));
        }
        for (version, name, body) in files {
            if applied.contains(&version) {
                continue;
            }
            let sql = std::str::from_utf8(body).map_err(|_| internal(format!("migration {name} is not UTF-8")))?;
            // A bare string has no bind arguments, so Postgres runs it over the simple-query protocol: several statements per file.
            Executor::execute(&mut *conn, sql).await.map_err(|e| internal(format!("migration {name}: {e}")))?;
            sqlx::query("INSERT INTO sr_migrations VALUES($1)").bind(version).execute(&mut *conn).await.map_err(db)?;
        }
        Ok(())
    }

    /// Begins a transaction that holds the tenant's advisory lock until it ends.
    async fn begin(&self, p: &Principal) -> StoreResult<Transaction<'static, Postgres>> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        let scope = serde_json::to_string(&[&p.workspace, &p.environment]).map_err(|e| internal(e.to_string()))?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))").bind(scope).execute(&mut *tx).await.map_err(db)?;
        Ok(tx)
    }

    async fn revalidate(&self, conn: &mut PgConnection, p: &Principal, scope: &str) -> StoreResult<()> {
        if let Some(r) = &self.opts.revalidate {
            r.revalidate(conn, p, scope).await?;
        }
        if !scope.is_empty() && !p.has_scope(scope) {
            return Err(fail("scope_denied", 403));
        }
        Ok(())
    }

    async fn row(&self, conn: &mut PgConnection, p: &Principal, id: &str) -> StoreResult<RequestRecord> {
        let row = sqlx::query(&format!(
            "SELECT {} FROM sr_requests WHERE workspace=$1 AND environment=$2 AND id=$3 AND customer=$4",
            request_columns()
        ))
        .bind(&p.workspace)
        .bind(&p.environment)
        .bind(id)
        .bind(&p.customer)
        .fetch_optional(&mut *conn)
        .await
        .map_err(db)?;
        let Some(row) = row else { return Err(fail("resource_not_found", 404)) };
        let (record, _) = scan_request(&p.workspace, &p.environment, &row)?;
        if !p.may_use(&record.service.id) {
            return Err(fail("scope_denied", 403));
        }
        Ok(record)
    }

    async fn request_by_key(&self, conn: &mut PgConnection, p: &Principal, key: &str) -> StoreResult<Option<(RequestRecord, String)>> {
        let row = sqlx::query(&format!("SELECT {} FROM sr_requests WHERE workspace=$1 AND environment=$2 AND idem=$3", request_columns()))
            .bind(&p.workspace)
            .bind(&p.environment)
            .bind(key)
            .fetch_optional(&mut *conn)
            .await
            .map_err(db)?;
        row.map(|r| scan_request(&p.workspace, &p.environment, &r)).transpose()
    }

    /// Adds a funding bucket with the full set of options.
    pub async fn grant_with(
        &self,
        p: &Principal,
        kind: &str,
        asset: &str,
        amount: &str,
        source: &str,
        opt: GrantOptions,
    ) -> StoreResult<String> {
        if !p.has_scope("billing:write") {
            return Err(fail("scope_denied", 403));
        }
        if !valid_amount(amount) {
            return Err(fail("invalid_amount", 400));
        }
        let mut tx = self.begin(p).await?;
        let result = async {
            self.revalidate(&mut tx, p, "billing:write").await?;
            Self::grant_tx(&mut tx, p, kind, asset, amount, source, opt).await
        }
        .await;
        conclude(tx, result).await
    }

    /// Adds a funding bucket inside a transaction the caller owns, after the caller has authorized it. Control planes
    /// use it to grant credits together with their own writes.
    pub async fn grant_tx(
        conn: &mut PgConnection,
        p: &Principal,
        kind: &str,
        asset: &str,
        amount: &str,
        source: &str,
        mut opt: GrantOptions,
    ) -> StoreResult<String> {
        if opt.source_kind.is_empty() {
            opt.source_kind = "purchased".to_string();
        }
        let owner = if kind == "platform_sale" { &p.workspace } else { &p.customer };
        let existing = sqlx::query(&format!(
            "SELECT b.account,a.asset,a.owner,a.kind,b.granted::text AS granted,{} AS expires_ms,to_json(b.service_ids)::text AS service_ids,b.source_kind \
             FROM sr_buckets b JOIN sr_accounts a ON (a.workspace,a.environment,a.id)=(b.workspace,b.environment,b.account) \
             WHERE b.workspace=$1 AND b.environment=$2 AND b.source=$3",
            ms("b.expires_at")
        ))
        .bind(&p.workspace)
        .bind(&p.environment)
        .bind(source)
        .fetch_optional(&mut *conn)
        .await
        .map_err(db)?;
        if let Some(b) = existing {
            let same = text(&b, "granted")? == amount
                && text(&b, "asset")? == asset
                && text(&b, "owner")? == *owner
                && text(&b, "kind")? == kind
                && b.try_get::<Option<i64>, _>("expires_ms").map_err(db)? == opt.expires
                && json_strings(opt_text(&b, "service_ids")?) == opt.service_ids
                && text(&b, "source_kind")? == opt.source_kind;
            return if same { text(&b, "account") } else { Err(fail("idempotency_conflict", 409)) };
        }
        sqlx::query(
            "INSERT INTO sr_accounts(workspace,environment,id,owner,kind,asset) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT(workspace,environment,owner,kind,asset) DO NOTHING",
        )
        .bind(&p.workspace)
        .bind(&p.environment)
        .bind(Uuid::new_v4().to_string())
        .bind(owner)
        .bind(kind)
        .bind(asset)
        .execute(&mut *conn)
        .await
        .map_err(db)?;
        let account: String =
            sqlx::query("SELECT id FROM sr_accounts WHERE workspace=$1 AND environment=$2 AND owner=$3 AND kind=$4 AND asset=$5")
                .bind(&p.workspace)
                .bind(&p.environment)
                .bind(owner)
                .bind(kind)
                .bind(asset)
                .fetch_one(&mut *conn)
                .await
                .map_err(db)?
                .try_get("id")
                .map_err(db)?;
        sqlx::query("UPDATE sr_accounts SET balance=balance+$4::numeric WHERE workspace=$1 AND environment=$2 AND id=$3")
            .bind(&p.workspace)
            .bind(&p.environment)
            .bind(&account)
            .bind(amount)
            .execute(&mut *conn)
            .await
            .map_err(db)?;
        sqlx::query(&format!(
            "INSERT INTO sr_buckets(workspace,environment,id,account,source,expires_at,remaining,granted,service_ids,source_kind) \
             VALUES($1,$2,$3,$4,$5,CASE WHEN $6::bigint IS NULL THEN NULL ELSE {} END,$7::numeric,$7::numeric,{},$9)",
            ts("$6"),
            array_expr("$8")
        ))
        .bind(&p.workspace)
        .bind(&p.environment)
        .bind(Uuid::new_v4().to_string())
        .bind(&account)
        .bind(source)
        .bind(opt.expires)
        .bind(amount)
        .bind(array_arg(opt.service_ids.as_deref()))
        .bind(&opt.source_kind)
        .execute(&mut *conn)
        .await
        .map_err(db)?;
        sqlx::query("INSERT INTO sr_journal(workspace,environment,business_key,asset,debit,credit,amount,reason) VALUES($1,$2,$3,$4,$5,$6,$7::numeric,$8)")
            .bind(&p.workspace)
            .bind(&p.environment)
            .bind(format!("grant:{source}"))
            .bind(asset)
            .bind(format!("issuer:{asset}"))
            .bind(&account)
            .bind(amount)
            .bind("grant")
            .execute(&mut *conn)
            .await
            .map_err(db)?;
        Ok(account)
    }

    /// Lets a customer use subscription-only services for a period (epoch milliseconds). `bucket` must be an existing
    /// bucket id. Control planes do this together with a subscription payment; this helper is for self-hosts and tests.
    #[allow(clippy::too_many_arguments)]
    pub async fn entitle(
        &self,
        p: &Principal,
        subscription: &str,
        cycle: i32,
        service_ids: &[String],
        start: i64,
        end: i64,
        bucket: &str,
    ) -> StoreResult<()> {
        self.entitle_with(p, subscription, cycle, service_ids, start, end, bucket, None).await
    }

    /// [`Self::entitle`] with the paid cycle's window limits snapshot (the plan's `window_limits` object with
    /// `enabled:true`), or `None` when the option is off.
    #[allow(clippy::too_many_arguments)]
    pub async fn entitle_with(
        &self,
        p: &Principal,
        subscription: &str,
        cycle: i32,
        service_ids: &[String],
        start: i64,
        end: i64,
        bucket: &str,
        window_limits: Option<&str>,
    ) -> StoreResult<()> {
        sqlx::query(&format!(
            "INSERT INTO sr_entitlements(workspace,environment,subscription,cycle,customer,service_ids,period_start,period_end,bucket,window_limits) \
             VALUES($1,$2,$3,$4,$5,{},{},{},$9,$10::jsonb)",
            array_expr("$6"),
            ts("$7"),
            ts("$8")
        ))
        .bind(&p.workspace)
        .bind(&p.environment)
        .bind(subscription)
        .bind(cycle)
        .bind(&p.customer)
        .bind(array_arg(Some(service_ids)))
        .bind(start)
        .bind(end)
        .bind(bucket)
        .bind(window_limits.filter(|w| !w.is_empty()))
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(())
    }

    /// A tenant's funding buckets ordered by source. For tests and diagnostics.
    pub async fn buckets(&self, p: &Principal) -> StoreResult<Vec<BucketRow>> {
        let rows = sqlx::query("SELECT source,remaining::text AS remaining,held::text AS held FROM sr_buckets WHERE workspace=$1 AND environment=$2 ORDER BY source")
            .bind(&p.workspace)
            .bind(&p.environment)
            .fetch_all(&self.pool)
            .await
            .map_err(db)?;
        rows.iter()
            .map(|r| Ok(BucketRow { source: text(r, "source")?, remaining: text(r, "remaining")?, held: text(r, "held")? }))
            .collect()
    }

    async fn event(conn: &mut PgConnection, p: &Principal, id: &str, kind: &str) -> StoreResult<()> {
        let event_id = Uuid::new_v4().to_string();
        let payload = json!({
            "id": event_id, "type": kind, "schema_version": 1, "workspace_id": p.workspace, "environment_id": p.environment,
            "request_id": id, "occurred_at": iso_millis(now_ms()),
        });
        sqlx::query("INSERT INTO sr_outbox(id,workspace,environment,request,payload) VALUES($1,$2,$3,$4,$5::jsonb)")
            .bind(&event_id)
            .bind(&p.workspace)
            .bind(&p.environment)
            .bind(id)
            .bind(payload.to_string())
            .execute(&mut *conn)
            .await
            .map_err(db)?;
        Ok(())
    }

    /// Moves the unheld remainder of expired buckets out of the balance.
    async fn expire(conn: &mut PgConnection, p: &Principal) -> StoreResult<()> {
        let rows = sqlx::query(
            "SELECT b.id,b.account,a.asset,(b.remaining-b.held)::text AS amount FROM sr_buckets b JOIN sr_accounts a ON (a.workspace,a.environment,a.id)=(b.workspace,b.environment,b.account) \
             WHERE b.workspace=$1 AND b.environment=$2 AND b.expires_at<=now() AND b.remaining>b.held ORDER BY b.id FOR UPDATE OF b",
        )
        .bind(&p.workspace)
        .bind(&p.environment)
        .fetch_all(&mut *conn)
        .await
        .map_err(db)?;
        for e in rows {
            let (id, account, asset, amount) = (text(&e, "id")?, text(&e, "account")?, text(&e, "asset")?, text(&e, "amount")?);
            sqlx::query("UPDATE sr_buckets SET remaining=held WHERE workspace=$1 AND environment=$2 AND id=$3")
                .bind(&p.workspace)
                .bind(&p.environment)
                .bind(&id)
                .execute(&mut *conn)
                .await
                .map_err(db)?;
            sqlx::query("UPDATE sr_accounts SET balance=balance-$4::numeric WHERE workspace=$1 AND environment=$2 AND id=$3")
                .bind(&p.workspace)
                .bind(&p.environment)
                .bind(&account)
                .bind(&amount)
                .execute(&mut *conn)
                .await
                .map_err(db)?;
            sqlx::query("INSERT INTO sr_journal(workspace,environment,business_key,asset,debit,credit,amount,reason) VALUES($1,$2,$3,$4,$5,$6,$7::numeric,$8)")
                .bind(&p.workspace)
                .bind(&p.environment)
                .bind(format!("expire:{}", Uuid::new_v4()))
                .bind(&asset)
                .bind(&account)
                .bind(format!("expired:{asset}"))
                .bind(&amount)
                .bind("expiry")
                .execute(&mut *conn)
                .await
                .map_err(db)?;
        }
        Ok(())
    }

    async fn finish_tx(
        &self,
        conn: &mut PgConnection,
        p: &Principal,
        id: &str,
        execution: &str,
        usage: Option<&Meters>,
        mut pending: bool,
    ) -> StoreResult<()> {
        let r = self.row(conn, p, id).await?;
        if r.billing == "settled" || r.billing == "released" {
            return Ok(());
        }
        let mut execution = execution.to_string();
        async fn exists(conn: &mut PgConnection, query: &str, p: &Principal, id: &str) -> StoreResult<bool> {
            let row = sqlx::query(query).bind(&p.workspace).bind(&p.environment).bind(id).fetch_optional(conn).await.map_err(db)?;
            Ok(row.is_some())
        }
        if pending
            && !r.output_started
            && !exists(conn, "SELECT 1 FROM sr_attempts WHERE workspace=$1 AND environment=$2 AND request=$3 LIMIT 1", p, id).await?
        {
            pending = false;
            execution = "cancelled".to_string();
        }
        if usage.is_some()
            && exists(
                conn,
                "SELECT 1 FROM sr_attempts WHERE workspace=$1 AND environment=$2 AND request=$3 AND cost>cost_bound LIMIT 1",
                p,
                id,
            )
            .await?
        {
            pending = true;
        }
        let mut due: Vec<BigInt> = Vec::with_capacity(r.legs.len());
        let mut exceeded = false;
        for leg in &r.legs {
            let mut amount = BigInt::zero();
            if let Some(u) = usage {
                amount = charge(&u.usage(), &leg.price)?;
                if amount > atoms(&leg.reserved)? {
                    exceeded = true;
                }
            }
            due.push(amount);
        }
        if pending || exceeded {
            if r.billing != "pending" {
                Self::event(conn, p, id, "reconciliation.required").await?;
            }
            sqlx::query("UPDATE sr_requests SET execution=$4,billing='pending' WHERE workspace=$1 AND environment=$2 AND id=$3")
                .bind(&p.workspace)
                .bind(&p.environment)
                .bind(id)
                .bind(&execution)
                .execute(&mut *conn)
                .await
                .map_err(db)?;
            let reason = if pending { "unknown_outcome" } else { "reservation_exceeded" };
            sqlx::query("INSERT INTO sr_reconciliation(workspace,environment,request,reason) VALUES($1,$2,$3,$4) ON CONFLICT DO NOTHING")
                .bind(&p.workspace)
                .bind(&p.environment)
                .bind(id)
                .bind(reason)
                .execute(&mut *conn)
                .await
                .map_err(db)?;
            return Ok(());
        }
        for (leg, due) in r.legs.iter().zip(&due) {
            let mut remaining = due.clone();
            let amount = due.to_string();
            let allocations = sqlx::query(
                "SELECT a.bucket,a.amount::text AS amount,to_json(a.windows)::text AS windows FROM sr_allocations a JOIN sr_buckets b ON (a.workspace,a.environment,a.bucket)=(b.workspace,b.environment,b.id) \
                 WHERE a.workspace=$1 AND a.environment=$2 AND a.request=$3 AND a.account=$4 \
                 ORDER BY b.expires_at ASC NULLS LAST, CASE b.source_kind WHEN 'subscription' THEN 0 WHEN 'promotion' THEN 1 ELSE 2 END,b.id",
            )
            .bind(&p.workspace)
            .bind(&p.environment)
            .bind(id)
            .bind(&leg.account)
            .fetch_all(&mut *conn)
            .await
            .map_err(db)?;
            for a in allocations {
                let (bucket, held_text) = (text(&a, "bucket")?, text(&a, "amount")?);
                let held = atoms(&held_text)?;
                let take = if held < remaining { held } else { remaining.clone() };
                remaining -= &take;
                let take_text = take.to_string();
                sqlx::query(
                    "UPDATE sr_allocations SET charged=$5::numeric WHERE workspace=$1 AND environment=$2 AND request=$3 AND bucket=$4",
                )
                .bind(&p.workspace)
                .bind(&p.environment)
                .bind(id)
                .bind(&bucket)
                .bind(&take_text)
                .execute(&mut *conn)
                .await
                .map_err(db)?;
                sqlx::query("UPDATE sr_buckets SET remaining=remaining-$4::numeric,held=held-$5::numeric WHERE workspace=$1 AND environment=$2 AND id=$3")
                    .bind(&p.workspace)
                    .bind(&p.environment)
                    .bind(&bucket)
                    .bind(&take_text)
                    .bind(&held_text)
                    .execute(&mut *conn)
                    .await
                    .map_err(db)?;
                let windows = json_strings(opt_text(&a, "windows")?).unwrap_or_default();
                settle_windows(conn, p, &windows, &held_text, &take_text).await?;
            }
            sqlx::query(
                "UPDATE sr_accounts SET balance=balance-$4::numeric,held=held-$5::numeric WHERE workspace=$1 AND environment=$2 AND id=$3",
            )
            .bind(&p.workspace)
            .bind(&p.environment)
            .bind(&leg.account)
            .bind(&amount)
            .bind(&leg.reserved)
            .execute(&mut *conn)
            .await
            .map_err(db)?;
            sqlx::query("INSERT INTO sr_settlements VALUES($1,$2,$3,$4,$5,$6::numeric)")
                .bind(&p.workspace)
                .bind(&p.environment)
                .bind(id)
                .bind(&leg.kind)
                .bind(&leg.account)
                .bind(&amount)
                .execute(&mut *conn)
                .await
                .map_err(db)?;
            sqlx::query("INSERT INTO sr_journal(workspace,environment,business_key,asset,debit,credit,amount,reason) VALUES($1,$2,$3,$4,$5,$6,$7::numeric,$8)")
                .bind(&p.workspace)
                .bind(&p.environment)
                .bind(format!("settle:{id}:{}", leg.kind))
                .bind(&leg.price.asset)
                .bind(&leg.account)
                .bind(format!("consumed:{}", leg.price.asset))
                .bind(&amount)
                .bind("settlement")
                .execute(&mut *conn)
                .await
                .map_err(db)?;
        }
        let (billing, kind) = if usage.is_some() { ("settled", "billing.settled") } else { ("released", "billing.released") };
        sqlx::query("UPDATE sr_requests SET execution=$4,billing=$5 WHERE workspace=$1 AND environment=$2 AND id=$3")
            .bind(&p.workspace)
            .bind(&p.environment)
            .bind(id)
            .bind(&execution)
            .bind(billing)
            .execute(&mut *conn)
            .await
            .map_err(db)?;
        Self::expire(conn, p).await?;
        Self::event(conn, p, id, kind).await
    }

    #[allow(clippy::too_many_arguments)]
    async fn reserve_tx(
        &self,
        conn: &mut PgConnection,
        p: &Principal,
        key: &str,
        digest: &str,
        svc: &Service,
        legs: &[Leg],
        deadline: i64,
        decision: &Decision,
    ) -> StoreResult<Reservation> {
        self.revalidate(conn, p, "invoke").await?;
        if !p.may_use(&svc.id) {
            return Err(fail("scope_denied", 403));
        }
        if svc.requires_subscription == Some(true) {
            let entitled = sqlx::query(
                "SELECT 1 FROM sr_entitlements WHERE workspace=$1 AND environment=$2 AND customer=$3 AND $4=ANY(service_ids) AND period_start<=now() AND period_end>now() LIMIT 1",
            )
            .bind(&p.workspace)
            .bind(&p.environment)
            .bind(&p.customer)
            .bind(&svc.id)
            .fetch_optional(&mut *conn)
            .await
            .map_err(db)?;
            if entitled.is_none() {
                return Err(fail("subscription_required", 403));
            }
        }
        if let Some((old, stored)) = self.request_by_key(conn, p, key).await? {
            if stored != digest || old.customer != p.customer {
                return Err(fail("idempotency_conflict", 409));
            }
            return Ok(Reservation { request: old, fresh: false });
        }
        // Admission and service publication share the tenant lock. Replays keep their admitted snapshot; fresh work
        // must reserve against the active version.
        let active =
            sqlx::query("SELECT 1 FROM sr_services WHERE workspace=$1 AND environment=$2 AND id=$3 AND version=$4 AND active=true")
                .bind(&p.workspace)
                .bind(&p.environment)
                .bind(&svc.id)
                .bind(svc.version)
                .fetch_optional(&mut *conn)
                .await
                .map_err(db)?;
        if active.is_none() {
            return Err(fail("inactive_service_version", 409));
        }
        let id = Uuid::new_v4().to_string();
        sqlx::query(&format!(
            "INSERT INTO sr_requests(workspace,environment,id,customer,idem,digest,service,legs,deadline,route_decision) VALUES($1,$2,$3,$4,$5,$6,$7::jsonb,$8::jsonb,{},$10::jsonb)",
            ts("$9")
        ))
        .bind(&p.workspace)
        .bind(&p.environment)
        .bind(&id)
        .bind(&p.customer)
        .bind(key)
        .bind(digest)
        .bind(json_text(svc)?)
        .bind(json_text(legs)?)
        .bind(deadline)
        .bind(json_text(decision)?)
        .execute(&mut *conn)
        .await
        .map_err(db)?;
        let mut ordered: Vec<&Leg> = legs.iter().collect();
        ordered.sort_by(|a, b| a.account.cmp(&b.account));
        for leg in ordered {
            let mut remaining = atoms(&leg.reserved)?;
            // A subscription bucket may carry session/weekly window limits: those of the latest active entitlement
            // part of the subscription that funds it. They gate only that bucket: once its windows are full the draw
            // falls through to the next bucket as for an empty one.
            let buckets = sqlx::query(
                "SELECT b.id,b.remaining::text AS remaining,b.held::text AS held,w.subscription,w.window_limits::text AS window_limits FROM sr_buckets b \
                 LEFT JOIN LATERAL (SELECT e.subscription,(SELECT e2.window_limits FROM sr_entitlements e2 WHERE (e2.workspace,e2.environment,e2.subscription)=(e.workspace,e.environment,e.subscription) AND e2.period_start<=now() AND e2.period_end>now() ORDER BY e2.cycle DESC,e2.part DESC LIMIT 1) AS window_limits \
                 FROM sr_entitlements e WHERE (e.workspace,e.environment,e.bucket)=(b.workspace,b.environment,b.id) LIMIT 1) w ON true \
                 WHERE b.workspace=$1 AND b.environment=$2 AND b.account=$3 AND (b.expires_at IS NULL OR b.expires_at>now()) AND (b.service_ids IS NULL OR $4=ANY(b.service_ids)) \
                 ORDER BY b.expires_at ASC NULLS LAST, CASE b.source_kind WHEN 'subscription' THEN 0 WHEN 'promotion' THEN 1 ELSE 2 END,b.id FOR UPDATE OF b",
            )
            .bind(&p.workspace)
            .bind(&p.environment)
            .bind(&leg.account)
            .bind(&svc.id)
            .fetch_all(&mut *conn)
            .await
            .map_err(db)?;
            let mut blocked: Option<WindowBlock> = None;
            for b in buckets {
                let bucket_id = text(&b, "id")?;
                let available = atoms(&text(&b, "remaining")?)? - atoms(&text(&b, "held")?)?;
                let mut take = if available >= remaining { remaining.clone() } else { available };
                if !take.is_positive() {
                    continue;
                }
                let mut window_ids: Vec<String> = Vec::new();
                if let Some(subscription) = opt_text(&b, "subscription")? {
                    let specs = window_specs(opt_text(&b, "window_limits")?.as_deref())?;
                    if !specs.is_empty() {
                        let admission = admit(conn, p, &subscription, &specs, &take).await?;
                        if admission.blocked.is_some() {
                            blocked = admission.blocked;
                        }
                        take = admission.take;
                        window_ids = admission.ids;
                        if !take.is_positive() {
                            continue;
                        }
                    }
                }
                sqlx::query("UPDATE sr_buckets SET held=held+$4::numeric WHERE workspace=$1 AND environment=$2 AND id=$3")
                    .bind(&p.workspace)
                    .bind(&p.environment)
                    .bind(&bucket_id)
                    .bind(take.to_string())
                    .execute(&mut *conn)
                    .await
                    .map_err(db)?;
                sqlx::query(&format!(
                    "INSERT INTO sr_allocations(workspace,environment,request,bucket,account,amount,windows) VALUES($1,$2,$3,$4,$5,$6::numeric,COALESCE({},'{{}}'))",
                    array_expr("$7")
                ))
                .bind(&p.workspace)
                .bind(&p.environment)
                .bind(&id)
                .bind(&bucket_id)
                .bind(&leg.account)
                .bind(take.to_string())
                .bind(array_arg(Some(&window_ids)))
                .execute(&mut *conn)
                .await
                .map_err(db)?;
                remaining -= &take;
                if remaining.is_zero() {
                    break;
                }
            }
            if remaining.is_positive() {
                return Err(match &blocked {
                    Some(gate) => window_error(gate),
                    None => fail("insufficient_balance", 402),
                });
            }
            sqlx::query("UPDATE sr_accounts SET held=held+$4::numeric WHERE workspace=$1 AND environment=$2 AND id=$3")
                .bind(&p.workspace)
                .bind(&p.environment)
                .bind(&leg.account)
                .bind(&leg.reserved)
                .execute(&mut *conn)
                .await
                .map_err(db)?;
        }
        let request = self.row(conn, p, &id).await?;
        Ok(Reservation { request, fresh: true })
    }
}

#[async_trait]
impl Store for PostgresStore {
    async fn replay(&self, p: &Principal, key: &str, digest: &str) -> StoreResult<Option<RequestRecord>> {
        let mut conn = self.pool.acquire().await.map_err(db)?;
        match self.request_by_key(&mut conn, p, key).await? {
            None => Ok(None),
            Some((rec, stored)) if stored == digest && rec.customer == p.customer => Ok(Some(rec)),
            Some(_) => Err(fail("idempotency_conflict", 409)),
        }
    }

    async fn get_service(&self, p: &Principal, id: &str, version: Option<i64>) -> StoreResult<Service> {
        if !p.may_use(id) {
            return Err(fail("scope_denied", 403));
        }
        let row = match version {
            None => {
                sqlx::query(
                    "SELECT config::text AS config FROM sr_services WHERE workspace=$1 AND environment=$2 AND id=$3 AND active=true",
                )
                .bind(&p.workspace)
                .bind(&p.environment)
                .bind(id)
                .fetch_optional(&self.pool)
                .await
            }
            Some(v) => {
                sqlx::query("SELECT config::text AS config FROM sr_services WHERE workspace=$1 AND environment=$2 AND id=$3 AND version=$4")
                    .bind(&p.workspace)
                    .bind(&p.environment)
                    .bind(id)
                    .bind(v)
                    .fetch_optional(&self.pool)
                    .await
            }
        }
        .map_err(db)?;
        let Some(row) = row else { return Err(fail("resource_not_found", 404)) };
        parse_stored(&text(&row, "config")?)
    }

    async fn accounts(&self, p: &Principal, svc: &Service) -> StoreResult<Vec<Leg>> {
        let mut legs = Vec::new();
        for (kind, price) in [("customer_sale", Some(&svc.price)), ("platform_sale", svc.platform_price.as_ref())] {
            let Some(price) = price else { continue };
            let owner = if kind == "platform_sale" { &p.workspace } else { &p.customer };
            let row = sqlx::query("SELECT id FROM sr_accounts WHERE workspace=$1 AND environment=$2 AND owner=$3 AND kind=$4 AND asset=$5")
                .bind(&p.workspace)
                .bind(&p.environment)
                .bind(owner)
                .bind(kind)
                .bind(&price.asset)
                .fetch_optional(&self.pool)
                .await
                .map_err(db)?;
            let Some(row) = row else { return Err(fail("insufficient_balance", 402)) };
            legs.push(Leg { kind: kind.to_string(), account: text(&row, "id")?, price: price.clone(), reserved: "0".to_string() });
        }
        Ok(legs)
    }

    async fn reserve(
        &self,
        p: &Principal,
        key: &str,
        digest: &str,
        svc: &Service,
        legs: &[Leg],
        deadline: i64,
        decision: &Decision,
    ) -> StoreResult<Reservation> {
        let mut tx = self.begin(p).await?;
        let result = self.reserve_tx(&mut tx, p, key, digest, svc, legs, deadline, decision).await;
        conclude(tx, result).await
    }

    async fn get(&self, p: &Principal, id: &str) -> StoreResult<RequestRecord> {
        let mut conn = self.pool.acquire().await.map_err(db)?;
        self.row(&mut conn, p, id).await
    }

    async fn begin(&self, p: &Principal, id: &str, o: &Offering, bound: &str) -> StoreResult<String> {
        let mut tx = self.begin(p).await?;
        let result = async {
            self.revalidate(&mut tx, p, "invoke").await?;
            let r = self.row(&mut tx, p, id).await?;
            if r.cancel_requested || (self.clock)() > r.deadline {
                return Err(fail("cancelled", 409));
            }
            if r.billing != "reserved" {
                return Err(fail("state_conflict", 409));
            }
            let disabled = sqlx::query("SELECT 1 FROM sr_disabled_connections WHERE workspace=$1 AND environment=$2 AND offering=$3")
                .bind(&p.workspace)
                .bind(&p.environment)
                .bind(&o.id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(db)?;
            if disabled.is_some() {
                return Err(fail("connection_disabled", 409));
            }
            let snapshot = r.service.all_offerings().into_iter().rfind(|x| x.id == o.id);
            if !snapshot.is_some_and(|s| s == o) {
                return Err(fail("state_conflict", 409));
            }
            let total: String = sqlx::query("SELECT (provider_spent+provider_held+$4::numeric)::text AS total FROM sr_requests WHERE workspace=$1 AND environment=$2 AND id=$3")
                .bind(&p.workspace)
                .bind(&p.environment)
                .bind(id)
                .bind(bound)
                .fetch_one(&mut *tx)
                .await
                .map_err(db)?
                .try_get("total")
                .map_err(db)?;
            if atoms(&total)? > atoms(&r.service.max_provider_cost)? {
                return Err(fail("budget_exceeded", 402));
            }
            let attempt = Uuid::new_v4().to_string();
            sqlx::query("INSERT INTO sr_attempts(workspace,environment,id,request,offering,cost_bound) VALUES($1,$2,$3,$4,$5,$6::numeric)")
                .bind(&p.workspace)
                .bind(&p.environment)
                .bind(&attempt)
                .bind(id)
                .bind(&o.id)
                .bind(bound)
                .execute(&mut *tx)
                .await
                .map_err(db)?;
            sqlx::query("UPDATE sr_requests SET execution='running',provider_held=provider_held+$4::numeric WHERE workspace=$1 AND environment=$2 AND id=$3")
                .bind(&p.workspace)
                .bind(&p.environment)
                .bind(id)
                .bind(bound)
                .execute(&mut *tx)
                .await
                .map_err(db)?;
            Ok(attempt)
        }
        .await;
        conclude(tx, result).await
    }

    async fn output(&self, p: &Principal, id: &str) -> StoreResult<()> {
        let mut tx = self.begin(p).await?;
        let result = async {
            self.row(&mut tx, p, id).await?;
            sqlx::query("UPDATE sr_requests SET output_started=true WHERE workspace=$1 AND environment=$2 AND id=$3")
                .bind(&p.workspace)
                .bind(&p.environment)
                .bind(id)
                .execute(&mut *tx)
                .await
                .map_err(db)?;
            Ok(())
        }
        .await;
        conclude(tx, result).await
    }

    async fn attempt_result(
        &self,
        p: &Principal,
        id: &str,
        attempt: &str,
        status: &str,
        usage: Option<&Meters>,
        cost: Option<&str>,
        upstream: &str,
    ) -> StoreResult<()> {
        let mut tx = self.begin(p).await?;
        let result = async {
            let r = self.row(&mut tx, p, id).await?;
            if r.billing != "reserved" && r.billing != "pending" {
                return Err(fail("state_conflict", 409));
            }
            let row = sqlx::query("SELECT status,cost_bound::text AS bound FROM sr_attempts WHERE workspace=$1 AND environment=$2 AND id=$3 AND request=$4 FOR UPDATE")
                .bind(&p.workspace)
                .bind(&p.environment)
                .bind(attempt)
                .bind(id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(db)?;
            let Some(row) = row else { return Err(fail("resource_not_found", 404)) };
            // The first recorded result of an attempt is final; later calls are ignored.
            if text(&row, "status")? != "dispatched" {
                return Ok(());
            }
            let bound = text(&row, "bound")?;
            let usage_json = usage.map(serde_json::to_string).transpose().map_err(|e| internal(e.to_string()))?;
            sqlx::query("UPDATE sr_attempts SET status=$4,usage=$5::jsonb,cost=$6::numeric,upstream_id=$7 WHERE workspace=$1 AND environment=$2 AND id=$3")
                .bind(&p.workspace)
                .bind(&p.environment)
                .bind(attempt)
                .bind(status)
                .bind(usage_json)
                .bind(cost)
                .bind(Some(upstream).filter(|u| !u.is_empty()))
                .execute(&mut *tx)
                .await
                .map_err(db)?;
            if let Some(cost) = cost {
                sqlx::query("UPDATE sr_requests SET provider_spent=provider_spent+$4::numeric,provider_held=provider_held-$5::numeric WHERE workspace=$1 AND environment=$2 AND id=$3")
                    .bind(&p.workspace)
                    .bind(&p.environment)
                    .bind(id)
                    .bind(cost)
                    .bind(&bound)
                    .execute(&mut *tx)
                    .await
                    .map_err(db)?;
            }
            Ok(())
        }
        .await;
        conclude(tx, result).await
    }

    async fn finish(&self, p: &Principal, id: &str, execution: &str, usage: Option<&Meters>, pending: bool) -> StoreResult<()> {
        let mut tx = self.begin(p).await?;
        let result = self.finish_tx(&mut tx, p, id, execution, usage, pending).await;
        conclude(tx, result).await
    }

    async fn cancel(&self, p: &Principal, id: &str) -> StoreResult<()> {
        let mut tx = self.begin(p).await?;
        let result = async {
            self.row(&mut tx, p, id).await?;
            sqlx::query("UPDATE sr_requests SET cancel_requested=true WHERE workspace=$1 AND environment=$2 AND id=$3")
                .bind(&p.workspace)
                .bind(&p.environment)
                .bind(id)
                .execute(&mut *tx)
                .await
                .map_err(db)?;
            Ok(())
        }
        .await;
        conclude(tx, result).await
    }
}

#[async_trait]
impl DataStore for PostgresStore {
    async fn publish(&self, p: &Principal, svc: &Service, mode: &str, expected_version: Option<i64>) -> StoreResult<Service> {
        if !p.has_scope("service:write") || !p.may_use(&svc.id) {
            return Err(fail("scope_denied", 403));
        }
        let mode = if mode.is_empty() { "self-host" } else { mode };
        if mode == "cloud" && svc.all_offerings().iter().any(|o| o.source == "byok") {
            return Err(fail("byok_not_allowed", 403));
        }
        let config = serde_json::to_string(svc).map_err(|e| internal(e.to_string()))?;
        let mut tx = self.begin(p).await?;
        let result = async {
            self.revalidate(&mut tx, p, "service:write").await?;
            let previous = sqlx::query("SELECT config::text AS config FROM sr_services WHERE workspace=$1 AND environment=$2 AND id=$3 ORDER BY version DESC LIMIT 1")
                .bind(&p.workspace)
                .bind(&p.environment)
                .bind(&svc.id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(db)?
                .map(|r| parse_stored(&text(&r, "config")?))
                .transpose()?;
            if expected_version.is_some() && previous.as_ref().map(|v| v.version) != expected_version {
                return Err(fail("service_version_conflict", 409));
            }
            if mode == "cloud" && !p.auth_key_id.is_empty() {
                // Database-authenticated developers cannot provision or rewrite platform supply economics.
                let protected = previous.as_ref().is_some_and(|prev| prev.platform_price == svc.platform_price && prev.all_offerings() == svc.all_offerings());
                if !protected {
                    return Err(fail("platform_pricing_protected", 403));
                }
            }
            let existing = sqlx::query("SELECT config::text AS config FROM sr_services WHERE workspace=$1 AND environment=$2 AND id=$3 AND version=$4")
                .bind(&p.workspace)
                .bind(&p.environment)
                .bind(&svc.id)
                .bind(svc.version)
                .fetch_optional(&mut *tx)
                .await
                .map_err(db)?;
            if let Some(row) = existing {
                return if parse_stored(&text(&row, "config")?)? == *svc { Ok(()) } else { Err(fail("version_exists", 409)) };
            }
            if previous.as_ref().is_some_and(|prev| svc.version <= prev.version) {
                return Err(fail("service_version_conflict", 409));
            }
            sqlx::query("UPDATE sr_services SET active=false WHERE workspace=$1 AND environment=$2 AND id=$3")
                .bind(&p.workspace)
                .bind(&p.environment)
                .bind(&svc.id)
                .execute(&mut *tx)
                .await
                .map_err(db)?;
            sqlx::query("INSERT INTO sr_services(workspace,environment,id,version,config) VALUES($1,$2,$3,$4,$5::jsonb)")
                .bind(&p.workspace)
                .bind(&p.environment)
                .bind(&svc.id)
                .bind(svc.version)
                .bind(&config)
                .execute(&mut *tx)
                .await
                .map_err(db)?;
            let actor = if p.member_id.is_empty() { "trusted-host" } else { p.member_id.as_str() };
            sqlx::query("INSERT INTO sr_audit(id,workspace,environment,actor,action,resource,details) VALUES($1,$2,$3,$4,$5,$6,$7::jsonb)")
                .bind(Uuid::new_v4().to_string())
                .bind(&p.workspace)
                .bind(&p.environment)
                .bind(actor)
                .bind("service.published")
                .bind(&svc.id)
                .bind(json!({ "version": svc.version }).to_string())
                .execute(&mut *tx)
                .await
                .map_err(db)?;
            Ok(())
        }
        .await;
        conclude(tx, result).await?;
        Ok(svc.clone())
    }

    async fn list(&self, p: &Principal) -> StoreResult<Vec<Service>> {
        let rows = sqlx::query(
            "SELECT config::text AS config FROM sr_services WHERE workspace=$1 AND environment=$2 AND active=true ORDER BY id LIMIT 100",
        )
        .bind(&p.workspace)
        .bind(&p.environment)
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        let mut out = Vec::new();
        for row in rows {
            let svc = parse_stored(&text(&row, "config")?)?;
            if p.may_use(&svc.id) {
                out.push(svc);
            }
        }
        Ok(out)
    }

    async fn balance(&self, p: &Principal) -> StoreResult<Vec<BalanceRow>> {
        let mut tx = self.begin(p).await?;
        let result = async {
            Self::expire(&mut tx, p).await?;
            let rows = sqlx::query(
                "SELECT id,kind,asset,balance::text AS balance,held::text AS held,(balance-held)::text AS available FROM sr_accounts \
                 WHERE workspace=$1 AND environment=$2 AND (owner=$3 OR (owner=$1 AND kind='platform_sale')) ORDER BY kind,id",
            )
            .bind(&p.workspace)
            .bind(&p.environment)
            .bind(&p.customer)
            .fetch_all(&mut *tx)
            .await
            .map_err(db)?;
            rows.iter()
                .map(|r| {
                    Ok(BalanceRow {
                        id: text(r, "id")?,
                        kind: text(r, "kind")?,
                        asset: text(r, "asset")?,
                        balance: text(r, "balance")?,
                        held: text(r, "held")?,
                        available: text(r, "available")?,
                    })
                })
                .collect::<StoreResult<Vec<_>>>()
        }
        .await;
        conclude(tx, result).await
    }

    async fn grant(&self, p: &Principal, kind: &str, asset: &str, amount: &str, source: &str, expires: Option<i64>) -> StoreResult<String> {
        self.grant_with(p, kind, asset, amount, source, GrantOptions { expires, ..GrantOptions::default() }).await
    }

    async fn disable(&self, p: &Principal, offering: &str) -> StoreResult<()> {
        if !p.has_scope("service:write") {
            return Err(fail("scope_denied", 403));
        }
        sqlx::query("INSERT INTO sr_disabled_connections VALUES($1,$2,$3) ON CONFLICT DO NOTHING")
            .bind(&p.workspace)
            .bind(&p.environment)
            .bind(offering)
            .execute(&self.pool)
            .await
            .map_err(db)?;
        Ok(())
    }

    /// Never redispatches an expired uncertain attempt.
    async fn recover(&self) -> StoreResult<usize> {
        let targets = sqlx::query(
            "SELECT workspace,environment,customer,id FROM sr_requests r WHERE deadline<now() AND billing='reserved' \
             AND NOT EXISTS (SELECT 1 FROM sr_tasks t WHERE (t.workspace,t.environment,t.id)=(r.workspace,r.environment,r.id)) ORDER BY deadline,id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        for t in &targets {
            let p = Principal {
                workspace: text(t, "workspace")?,
                environment: text(t, "environment")?,
                customer: text(t, "customer")?,
                ..Principal::default()
            };
            let id = text(t, "id")?;
            let mut tx = self.begin(&p).await?;
            let result = async {
                let r = self.row(&mut tx, &p, &id).await?;
                if r.billing != "reserved" {
                    return Ok(());
                }
                let attempts = sqlx::query("SELECT status,usage::text AS usage FROM sr_attempts WHERE workspace=$1 AND environment=$2 AND request=$3 ORDER BY created_at,id")
                    .bind(&p.workspace)
                    .bind(&p.environment)
                    .bind(&id)
                    .fetch_all(&mut *tx)
                    .await
                    .map_err(db)?;
                let last = attempts.last();
                let last_status = last.map(|a| text(a, "status")).transpose()?.unwrap_or_default();
                let last_usage = last.map(|a| opt_text(a, "usage")).transpose()?.flatten().filter(|u| !u.is_empty() && u != "null");
                let known = last.is_some() && (last_status == "succeeded" || last_status == "partial") && last_usage.is_some();
                let (execution, usage) = if known {
                    let meters: Meters = serde_json::from_str(last_usage.as_deref().unwrap_or("{}")).map_err(|e| internal(format!("stored usage: {e}")))?;
                    (last_status, Some(meters))
                } else if last.is_some() {
                    ("unknown".to_string(), None)
                } else {
                    ("cancelled".to_string(), None)
                };
                self.finish_tx(&mut tx, &p, &id, &execution, usage.as_ref(), last.is_some() && !known).await
            }
            .await;
            conclude(tx, result).await?;
        }
        Ok(targets.len())
    }
}
