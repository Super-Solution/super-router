//! Rebate arithmetic: exact integers, rounded **down** (what we give away rounds the other way from what we charge).

/// 100% in basis points.
pub const MAX_BPS: i32 = 10_000;

/// Whether `bps` is a rate a rule may have: 0 to 100%.
pub fn valid_bps(bps: i32) -> bool {
    (0..=MAX_BPS).contains(&bps)
}

/// `floor(base * bps / 10_000)`, in `i128` so no input overflows; a negative base or an invalid rate gives 0.
pub fn rebate(base: i64, bps: i32) -> i64 {
    if base <= 0 || !valid_bps(bps) {
        return 0;
    }
    let v = i128::from(base) * i128::from(bps) / i128::from(MAX_BPS);
    i64::try_from(v).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounds_down_and_never_overflows() {
        assert_eq!(rebate(1000, 300), 30);
        assert_eq!(rebate(1000, 500), 50);
        assert_eq!(rebate(33, 300), 0, "0.99 is nothing");
        assert_eq!(rebate(34, 300), 1);
        assert_eq!(rebate(i64::MAX, 10_000), i64::MAX);
        assert_eq!(rebate(i64::MAX, 1), i64::MAX / 10_000);
        assert_eq!(rebate(-5, 300), 0);
        assert_eq!(rebate(1000, 10_001), 0);
        assert_eq!(rebate(1000, -1), 0);
    }
}
