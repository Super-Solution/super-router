//! Optional session and weekly usage windows on subscription credits (docs/WINDOW_LIMITS.md).

use num_bigint::BigInt;
use num_traits::{Signed, Zero};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use sqlx::{PgConnection, Row};
use super_router_contracts::{Principal, RouterError};
use super_router_router::{StoreError, StoreResult};
use uuid::Uuid;

use crate::util::{atoms, db, iso_millis, ms, text};
use crate::PostgresStore;

/// The fixed length of the long window; only the session window's length is configurable.
const WEEK_HOURS: i32 = 168;

/// One enforced window of an entitlement's snapshot.
#[derive(Debug, Clone)]
pub(crate) struct WindowSpec {
    pub kind: &'static str,
    pub hours: i32,
    pub limit: BigInt,
}

/// A window that held a draw below what was wanted.
#[derive(Debug, Clone)]
pub(crate) struct WindowBlock {
    kind: &'static str,
    limit: BigInt,
    used: BigInt,
    /// When the open window closes (epoch milliseconds); `None` when no window is open (the draw alone exceeds the limit).
    resets_at: Option<i64>,
    /// False when even an empty window could not admit the draw, so waiting does not help.
    retryable: bool,
}

impl WindowBlock {
    /// Which of two blocking windows to report: one that waiting cannot fix, else the one that resets last.
    fn worse(&self, than: Option<&WindowBlock>) -> bool {
        let Some(than) = than else { return true };
        match (self.retryable, than.retryable) {
            (false, true) => true,
            (a, b) if a != b => false,
            _ => self.resets_at.unwrap_or(i64::MIN) > than.resets_at.unwrap_or(i64::MIN),
        }
    }
}

#[derive(Deserialize)]
struct Session {
    hours: i32,
    credits: String,
}

#[derive(Deserialize)]
struct Weekly {
    credits: String,
}

#[derive(Deserialize)]
struct Snapshot {
    #[serde(default)]
    enabled: bool,
    session: Option<Session>,
    weekly: Option<Weekly>,
}

/// Reads an entitlement's `window_limits` snapshot. Empty when the option is off or absent.
pub(crate) fn window_specs(raw: Option<&str>) -> StoreResult<Vec<WindowSpec>> {
    let Some(raw) = raw.filter(|r| !r.is_empty() && *r != "null") else { return Ok(Vec::new()) };
    let snapshot: Snapshot = serde_json::from_str(raw).map_err(|e| StoreError::Internal(format!("window_limits: {e}")))?;
    if !snapshot.enabled {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    if let Some(s) = snapshot.session {
        out.push(WindowSpec { kind: "session", hours: s.hours, limit: atoms(&s.credits)? });
    }
    if let Some(w) = snapshot.weekly {
        out.push(WindowSpec { kind: "weekly", hours: WEEK_HOURS, limit: atoms(&w.credits)? });
    }
    Ok(out)
}

struct Open {
    id: String,
    end: i64,
    spent: BigInt,
    held: BigInt,
}

/// What [`admit`] decided.
pub(crate) struct Admission {
    /// How much may be drawn: the smallest headroom.
    pub take: BigInt,
    /// The windows that now hold it.
    pub ids: Vec<String>,
    /// When a window held the draw below what was wanted, the one to report.
    pub blocked: Option<WindowBlock>,
}

/// Admits up to `want` of the subscription's credits against every enforced window. Windows open lazily and only
/// when something is actually drawn. It runs inside the tenant transaction, so concurrent reservations of one
/// tenant cannot overshoot a limit.
pub(crate) async fn admit(
    conn: &mut PgConnection,
    p: &Principal,
    subscription: &str,
    specs: &[WindowSpec],
    want: &BigInt,
) -> StoreResult<Admission> {
    let mut open: Vec<Option<Open>> = Vec::with_capacity(specs.len());
    let mut take = want.clone();
    let mut blocked: Option<WindowBlock> = None;
    let window_end = ms("window_end");
    for spec in specs {
        let row = sqlx::query(&format!(
            "SELECT id,{window_end} AS window_end,spent::text AS spent,held::text AS held FROM sr_usage_windows \
             WHERE workspace=$1 AND environment=$2 AND subscription=$3 AND kind=$4 AND window_end>now() ORDER BY window_start DESC LIMIT 1 FOR UPDATE"
        ))
        .bind(&p.workspace)
        .bind(&p.environment)
        .bind(subscription)
        .bind(spec.kind)
        .fetch_optional(&mut *conn)
        .await
        .map_err(db)?;
        let current = match row {
            Some(r) => Some(Open {
                id: text(&r, "id")?,
                end: r.try_get("window_end").map_err(db)?,
                spent: atoms(&text(&r, "spent")?)?,
                held: atoms(&text(&r, "held")?)?,
            }),
            None => None,
        };
        let used = current.as_ref().map_or_else(BigInt::zero, |o| &o.spent + &o.held);
        let room = if spec.limit > used { &spec.limit - &used } else { BigInt::zero() };
        if room < *want {
            let block = WindowBlock {
                kind: spec.kind,
                limit: spec.limit.clone(),
                used,
                resets_at: current.as_ref().map(|o| o.end),
                retryable: *want <= spec.limit,
            };
            if block.worse(blocked.as_ref()) {
                blocked = Some(block);
            }
            if room < take {
                take = room;
            }
        }
        open.push(current);
    }
    if !take.is_positive() {
        return Ok(Admission { take: BigInt::zero(), ids: Vec::new(), blocked });
    }
    let mut ids = Vec::with_capacity(specs.len());
    for (spec, current) in specs.iter().zip(open) {
        let id = match current {
            Some(o) => o.id,
            None => {
                let id = Uuid::new_v4().to_string();
                sqlx::query(
                    "INSERT INTO sr_usage_windows(workspace,environment,id,subscription,kind,window_start,window_end) \
                     VALUES($1,$2,$3,$4,$5,now(),now()+make_interval(hours=>$6))",
                )
                .bind(&p.workspace)
                .bind(&p.environment)
                .bind(&id)
                .bind(subscription)
                .bind(spec.kind)
                .bind(spec.hours)
                .execute(&mut *conn)
                .await
                .map_err(db)?;
                id
            }
        };
        sqlx::query("UPDATE sr_usage_windows SET held=held+$4::numeric WHERE workspace=$1 AND environment=$2 AND id=$3")
            .bind(&p.workspace)
            .bind(&p.environment)
            .bind(&id)
            .bind(take.to_string())
            .execute(&mut *conn)
            .await
            .map_err(db)?;
        ids.push(id);
    }
    Ok(Admission { take, ids, blocked })
}

/// Moves an allocation's hold out of its windows and what was actually charged into them. A window left with
/// nothing spent and nothing held (every request in it was released) is dropped, so a request that cost nothing
/// does not start anyone's session clock.
pub(crate) async fn settle_windows(conn: &mut PgConnection, p: &Principal, ids: &[String], held: &str, charged: &str) -> StoreResult<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let list = serde_json::to_string(ids).map_err(|e| StoreError::Internal(e.to_string()))?;
    sqlx::query(
        "UPDATE sr_usage_windows SET held=held-$4::numeric,spent=spent+$5::numeric WHERE workspace=$1 AND environment=$2 AND id=ANY(SELECT jsonb_array_elements_text($3::jsonb))",
    )
    .bind(&p.workspace)
    .bind(&p.environment)
    .bind(&list)
    .bind(held)
    .bind(charged)
    .execute(&mut *conn)
    .await
    .map_err(db)?;
    sqlx::query("DELETE FROM sr_usage_windows WHERE workspace=$1 AND environment=$2 AND id=ANY(SELECT jsonb_array_elements_text($3::jsonb)) AND spent=0 AND held=0")
        .bind(&p.workspace)
        .bind(&p.environment)
        .bind(&list)
        .execute(&mut *conn)
        .await
        .map_err(db)?;
    Ok(())
}

/// The customer-facing refusal: which window, how full, when it resets.
pub(crate) fn window_error(b: &WindowBlock) -> StoreError {
    let mut details = Map::new();
    details.insert("window".into(), json!(b.kind));
    details.insert("limit".into(), json!(b.limit.to_string()));
    details.insert("used".into(), json!(b.used.to_string()));
    details.insert("resets_at".into(), b.resets_at.map_or(Value::Null, |t| json!(iso_millis(t))));
    let mut e = RouterError::new("window_limit_reached", 429);
    e.retryable = b.retryable;
    e.details = Some(details);
    StoreError::Router(e)
}

/// One session or weekly window of a paid subscription, as [`PostgresStore::windows`] reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowRow {
    pub subscription: String,
    pub window: String,
    pub hours: i32,
    pub limit: String,
    pub spent: String,
    pub held: String,
    pub remaining: String,
    /// Epoch milliseconds; `None` for a window that has not opened yet.
    pub window_start: Option<i64>,
    pub resets_at: Option<i64>,
}

impl PostgresStore {
    /// The customer's session/weekly window usage for subscriptions with the option on. The limits are those of each
    /// subscription's latest active entitlement part. A window that has not opened yet reports nothing spent and no
    /// reset time: it starts with the next request.
    pub async fn windows(&self, p: &Principal) -> StoreResult<Vec<WindowRow>> {
        let entitlements = sqlx::query(
            "SELECT DISTINCT ON (subscription) subscription,window_limits::text AS window_limits FROM sr_entitlements \
             WHERE workspace=$1 AND environment=$2 AND customer=$3 AND period_start<=now() AND period_end>now() ORDER BY subscription,cycle DESC,part DESC",
        )
        .bind(&p.workspace)
        .bind(&p.environment)
        .bind(&p.customer)
        .fetch_all(self.pool())
        .await
        .map_err(db)?;
        let (start, end) = (ms("window_start"), ms("window_end"));
        let mut out = Vec::new();
        for e in entitlements {
            let subscription = text(&e, "subscription")?;
            let limits = e.try_get::<Option<String>, _>("window_limits").map_err(db)?;
            for spec in window_specs(limits.as_deref())? {
                let mut row = WindowRow {
                    subscription: subscription.clone(),
                    window: spec.kind.to_string(),
                    hours: spec.hours,
                    limit: spec.limit.to_string(),
                    spent: "0".into(),
                    held: "0".into(),
                    remaining: String::new(),
                    window_start: None,
                    resets_at: None,
                };
                let current = sqlx::query(&format!(
                    "SELECT {start} AS window_start,{end} AS window_end,spent::text AS spent,held::text AS held FROM sr_usage_windows \
                     WHERE workspace=$1 AND environment=$2 AND subscription=$3 AND kind=$4 AND window_end>now() ORDER BY window_start DESC LIMIT 1"
                ))
                .bind(&p.workspace)
                .bind(&p.environment)
                .bind(&subscription)
                .bind(spec.kind)
                .fetch_optional(self.pool())
                .await
                .map_err(db)?;
                let mut used = BigInt::zero();
                if let Some(w) = current {
                    row.spent = text(&w, "spent")?;
                    row.held = text(&w, "held")?;
                    row.window_start = Some(w.try_get("window_start").map_err(db)?);
                    row.resets_at = Some(w.try_get("window_end").map_err(db)?);
                    used = atoms(&row.spent)? + atoms(&row.held)?;
                }
                let remaining = if spec.limit > used { &spec.limit - &used } else { BigInt::zero() };
                row.remaining = remaining.to_string();
                out.push(row);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshots_off_or_absent_enforce_nothing() {
        for raw in [None, Some(""), Some("null"), Some(r#"{"enabled":false,"session":{"hours":5,"credits":"1"}}"#), Some("{}")] {
            assert!(window_specs(raw).unwrap().is_empty(), "{raw:?}");
        }
    }

    #[test]
    fn snapshots_on_yield_the_configured_windows() {
        let specs = window_specs(Some(r#"{"enabled":true,"session":{"hours":5,"credits":"2250"},"weekly":{"credits":"2400"}}"#)).unwrap();
        assert_eq!(specs.len(), 2);
        assert_eq!((specs[0].kind, specs[0].hours, specs[0].limit.to_string()), ("session", 5, "2250".into()));
        assert_eq!((specs[1].kind, specs[1].hours, specs[1].limit.to_string()), ("weekly", 168, "2400".into()));
        assert!(window_specs(Some(r#"{"enabled":true,"session":{"hours":5,"credits":"x"}}"#)).is_err());
    }

    #[test]
    fn the_unfixable_or_latest_resetting_window_is_reported() {
        let block =
            |retryable, resets_at| WindowBlock { kind: "session", limit: BigInt::zero(), used: BigInt::zero(), resets_at, retryable };
        assert!(block(true, Some(1)).worse(None));
        assert!(block(false, None).worse(Some(&block(true, Some(9)))));
        assert!(!block(true, Some(9)).worse(Some(&block(false, None))));
        assert!(block(true, Some(9)).worse(Some(&block(true, Some(1)))));
        assert!(!block(true, Some(1)).worse(Some(&block(true, Some(9)))));
    }
}
