//! A strict UTC timestamp reader: `YYYY-MM-DDTHH:MM:SS[.fraction]Z`, the only form the contracts accept.

/// Milliseconds since the Unix epoch of a valid UTC timestamp, rounded toward negative infinity.
pub fn parse_utc_millis(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    // YYYY-MM-DDTHH:MM:SS is 19 bytes, then an optional fraction, then Z.
    if b.len() < 20 || *b.last()? != b'Z' {
        return None;
    }
    let digits = |from: usize, to: usize| -> Option<i64> {
        let part = b.get(from..to)?;
        part.iter().all(u8::is_ascii_digit).then(|| part.iter().fold(0i64, |n, d| n * 10 + i64::from(d - b'0')))
    };
    if b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let (year, month, day) = (digits(0, 4)?, digits(5, 7)?, digits(8, 10)?);
    let (hour, minute, second) = (digits(11, 13)?, digits(14, 16)?, digits(17, 19)?);
    let mut millis = 0;
    match &b[19..b.len() - 1] {
        [] => {}
        [b'.', frac @ ..] if !frac.is_empty() && frac.iter().all(u8::is_ascii_digit) => {
            for i in 0..3 {
                millis = millis * 10 + frac.get(i).map_or(0, |d| i64::from(d - b'0'));
            }
        }
        _ => return None,
    }
    if !(1..=12).contains(&month) || day < 1 || day > days_in_month(year, month) || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let days = days_from_civil(year, month, day);
    Some(((days * 24 + hour) * 60 + minute) * 60_000 + second * 1000 + millis)
}

/// Whether `s` is a timestamp [`parse_utc_millis`] accepts.
pub fn valid_datetime(s: &str) -> bool {
    parse_utc_millis(s).is_some()
}

fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        2 if is_leap(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_known_instants() {
        assert_eq!(parse_utc_millis("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_utc_millis("2000-01-01T00:00:00Z"), Some(946_684_800_000));
        assert_eq!(parse_utc_millis("2026-11-01T00:00:00.123456Z"), Some(1_793_491_200_123));
        assert_eq!(parse_utc_millis("2024-02-29T12:00:00Z"), Some(1_709_208_000_000));
    }

    #[test]
    fn rejects_everything_else() {
        for bad in [
            "",
            "2026-11-01",
            "2026-11-01T00:00:00",
            "2026-11-01T00:00:00+00:00",
            "2026-11-01 00:00:00Z",
            "2026-11-01T00:00:00.Z",
            "2026-13-01T00:00:00Z",
            "2026-02-30T00:00:00Z",
            "2025-02-29T00:00:00Z",
            "2026-11-01T24:00:00Z",
            "2026-11-01T00:60:00Z",
            "2026-11-01T00:00:60Z",
            "2026-11-01t00:00:00Z",
            "2026-11-01T00:00:00z",
            "٢٠٢٦-11-01T00:00:00Z",
        ] {
            assert!(!valid_datetime(bad), "{bad}");
        }
    }
}
