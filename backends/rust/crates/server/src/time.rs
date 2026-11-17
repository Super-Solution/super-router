//! RFC 3339 timestamps with an offset, as the grant endpoint accepts them.

use super_router_contracts::parse_utc_millis;

/// Milliseconds since the epoch of `YYYY-MM-DDTHH:MM:SS[.fraction](Z|±HH:MM)`.
pub(crate) fn parse_rfc3339_millis(s: &str) -> Option<i64> {
    if s.ends_with('Z') {
        return parse_utc_millis(s);
    }
    let b = s.as_bytes();
    let n = b.len();
    if n < 6 || !matches!(b[n - 6], b'+' | b'-') || b[n - 3] != b':' {
        return None;
    }
    let two = |at: usize| -> Option<i64> {
        let part = b.get(at..at + 2)?;
        part.iter().all(u8::is_ascii_digit).then(|| i64::from(part[0] - b'0') * 10 + i64::from(part[1] - b'0'))
    };
    let (hours, minutes) = (two(n - 5)?, two(n - 2)?);
    if hours > 23 || minutes > 59 {
        return None;
    }
    let local = parse_utc_millis(&format!("{}Z", &s[..n - 6]))?;
    let offset = (hours * 60 + minutes) * 60_000;
    Some(if b[n - 6] == b'+' { local - offset } else { local + offset })
}

#[cfg(test)]
mod tests {
    use super::parse_rfc3339_millis as parse;

    #[test]
    fn reads_zulu_and_offsets() {
        assert_eq!(parse("2030-01-01T00:00:00Z"), Some(1_893_456_000_000));
        assert_eq!(parse("2030-01-01T08:00:00+08:00"), Some(1_893_456_000_000));
        assert_eq!(parse("2029-12-31T19:30:00-04:30"), Some(1_893_456_000_000));
        assert_eq!(parse("2030-01-01T00:00:00.5+00:00"), Some(1_893_456_000_500));
    }

    #[test]
    fn rejects_the_rest() {
        for bad in [
            "",
            "2030-01-01",
            "2030-01-01T00:00:00",
            "2030-01-01T00:00:00+0800",
            "2030-01-01T00:00:00+24:00",
            "2030-01-01T00:00:00+08:60",
            "nope",
        ] {
            assert_eq!(parse(bad), None, "{bad}");
        }
    }
}
