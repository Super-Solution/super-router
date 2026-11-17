//! Small helpers shared by the store: error mapping, the millisecond/timestamp boundary and JSON text.

use num_bigint::BigInt;
use sqlx::postgres::PgRow;
use sqlx::Row;
use super_router_contracts::RouterError;
use super_router_router::StoreError;

/// A database failure is internal: its text is for logs and never reaches a customer.
pub(crate) fn db(e: sqlx::Error) -> StoreError {
    StoreError::Internal(e.to_string())
}

pub(crate) fn fail(code: &str, status: u16) -> StoreError {
    StoreError::Router(RouterError::new(code, status))
}

pub(crate) fn internal(message: impl Into<String>) -> StoreError {
    StoreError::Internal(message.into())
}

/// A decimal atom amount read back from a `numeric(38,0)` column.
pub(crate) fn atoms(s: &str) -> Result<BigInt, StoreError> {
    super_router_pricing::atoms(s).map_err(|_| internal(format!("not a decimal integer: {s}")))
}

/// SQL that turns an epoch-millisecond bind parameter into a `timestamptz`, exactly: integer arithmetic only.
pub(crate) fn ts(param: &str) -> String {
    format!("('epoch'::timestamptz + {param}::bigint * interval '1 millisecond')")
}

/// SQL that reads a `timestamptz` column as epoch milliseconds, rounded down.
pub(crate) fn ms(column: &str) -> String {
    format!("floor(extract(epoch from {column}) * 1000)::bigint")
}

/// A Postgres `text[]` built from a JSON array parameter: `(SELECT array_agg(x) FROM jsonb_array_elements_text($n::jsonb) x)`.
/// An empty list yields NULL, as in the Go store, so "no service restriction" and "empty list" both read back as NULL.
pub(crate) fn array_expr(param: &str) -> String {
    format!("(SELECT array_agg(x) FROM jsonb_array_elements_text({param}::jsonb) x)")
}

/// The JSON text of a list, or `None` for "no list" (SQL NULL).
pub(crate) fn array_arg(list: Option<&[String]>) -> Option<String> {
    list.map(|l| serde_json::to_string(l).unwrap_or_else(|_| "[]".to_string()))
}

/// The JSON text of a value the store persists as `jsonb`.
pub(crate) fn json_text<T: serde::Serialize + ?Sized>(v: &T) -> Result<String, StoreError> {
    serde_json::to_string(v).map_err(|e| internal(e.to_string()))
}

/// A `text[]` column selected as `to_json(col)::text`.
pub(crate) fn json_strings(raw: Option<String>) -> Option<Vec<String>> {
    raw.filter(|r| r != "null").and_then(|r| serde_json::from_str(&r).ok())
}

pub(crate) fn text(row: &PgRow, column: &str) -> Result<String, StoreError> {
    row.try_get::<String, _>(column).map_err(db)
}

pub(crate) fn opt_text(row: &PgRow, column: &str) -> Result<Option<String>, StoreError> {
    row.try_get::<Option<String>, _>(column).map_err(db)
}

/// Milliseconds since the epoch, now.
pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// `YYYY-MM-DDTHH:MM:SS.mmmZ`, the form the reference prints instants in.
pub fn iso_millis(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let milli = ms.rem_euclid(1000);
    let (days, rest) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{milli:03}Z", rest / 3600, rest % 3600 / 60, rest % 60)
}

/// The proleptic Gregorian date of a day count since 1970-01-01 (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instants_print_like_the_reference() {
        assert_eq!(iso_millis(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso_millis(1_800_000_000_123), "2027-01-15T08:00:00.123Z");
        assert_eq!(iso_millis(951_782_400_000), "2000-02-29T00:00:00.000Z");
        assert_eq!(iso_millis(-1), "1969-12-31T23:59:59.999Z");
    }

    #[test]
    fn lists_round_trip_through_sql_text() {
        assert_eq!(array_arg(None), None);
        assert_eq!(array_arg(Some(&["a".to_string(), "b".to_string()])).as_deref(), Some(r#"["a","b"]"#));
        assert_eq!(json_strings(None), None);
        assert_eq!(json_strings(Some("null".into())), None);
        assert_eq!(json_strings(Some(r#"["x"]"#.into())), Some(vec!["x".to_string()]));
        assert_eq!(json_strings(Some("[]".into())), Some(vec![]));
    }
}
