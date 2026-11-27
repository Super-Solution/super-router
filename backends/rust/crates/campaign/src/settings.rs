use crate::error::{invalid, Error};
use crate::rebate::valid_bps;
use crate::store::Campaigns;
use crate::types::{RateClass, Settings};

impl Campaigns {
    pub async fn settings(&self) -> Result<Settings, Error> {
        Ok(sqlx::query_as::<_, Settings>(
            "SELECT referral_enabled, hold_days, attach_window_hours, rebate_window_days, rebate_cap, max_per_signal,
                    code_fail_limit, code_fail_window_minutes, updated_at
               FROM sc_settings WHERE id",
        )
        .fetch_one(&self.pool)
        .await?)
    }

    /// Replaces the settings after checking every bound (the table's checks are the second line).
    pub async fn set_settings(&self, s: &Settings, now_ms: i64) -> Result<Settings, Error> {
        let ok = (0..=90).contains(&s.hold_days)
            && (1..=8760).contains(&s.attach_window_hours)
            && s.rebate_window_days.is_none_or(|d| (1..=3650).contains(&d))
            && s.rebate_cap.is_none_or(|c| c > 0)
            && (1..=1000).contains(&s.max_per_signal)
            && (1..=100).contains(&s.code_fail_limit)
            && (1..=1440).contains(&s.code_fail_window_minutes);
        if !ok {
            return invalid("a setting is out of its range");
        }
        sqlx::query(
            "UPDATE sc_settings SET referral_enabled=$1, hold_days=$2, attach_window_hours=$3, rebate_window_days=$4,
                    rebate_cap=$5, max_per_signal=$6, code_fail_limit=$7, code_fail_window_minutes=$8, updated_at=$9
              WHERE id",
        )
        .bind(s.referral_enabled)
        .bind(s.hold_days)
        .bind(s.attach_window_hours)
        .bind(s.rebate_window_days)
        .bind(s.rebate_cap)
        .bind(s.max_per_signal)
        .bind(s.code_fail_limit)
        .bind(s.code_fail_window_minutes)
        .bind(now_ms)
        .execute(&self.pool)
        .await?;
        self.settings().await
    }

    pub async fn rate_classes(&self) -> Result<Vec<RateClass>, Error> {
        Ok(sqlx::query_as::<_, RateClass>(
            "SELECT c.name, c.rate_bps, c.is_default, c.updated_at,
                    (SELECT count(*) FROM sc_referrers r WHERE r.class = c.name) AS members
               FROM sc_rate_classes c ORDER BY c.is_default DESC, c.name",
        )
        .fetch_all(&self.pool)
        .await?)
    }

    /// Makes the class when it does not exist yet and leaves an existing one (and its rate) alone: for a host that
    /// wants a second class from the start without overwriting what an administrator later set.
    pub async fn ensure_rate_class(&self, name: &str, rate_bps: i32, now_ms: i64) -> Result<(), Error> {
        check_class(name, rate_bps)?;
        sqlx::query("INSERT INTO sc_rate_classes (name, rate_bps, updated_at) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING")
            .bind(name)
            .bind(rate_bps)
            .bind(now_ms)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Sets a class's rate (creating it when new). The default class cannot be removed or renamed, only re-rated.
    /// A rate change applies to purchases after it; a rebate keeps the rate it was decided at.
    pub async fn set_rate(&self, name: &str, rate_bps: i32, now_ms: i64) -> Result<(), Error> {
        check_class(name, rate_bps)?;
        sqlx::query(
            "INSERT INTO sc_rate_classes (name, rate_bps, updated_at) VALUES ($1, $2, $3)
             ON CONFLICT (name) DO UPDATE SET rate_bps = EXCLUDED.rate_bps, updated_at = EXCLUDED.updated_at",
        )
        .bind(name)
        .bind(rate_bps)
        .bind(now_ms)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Removes a class nobody is in. The default class is refused, and so is one that has members.
    pub async fn remove_rate_class(&self, name: &str) -> Result<bool, Error> {
        let r = sqlx::query(
            "DELETE FROM sc_rate_classes c WHERE c.name = $1 AND NOT c.is_default
               AND NOT EXISTS (SELECT 1 FROM sc_referrers r WHERE r.class = c.name)",
        )
        .bind(name)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() > 0)
    }
}

fn check_class(name: &str, rate_bps: i32) -> Result<(), Error> {
    let mut chars = name.chars();
    let name_ok = name.len() <= 32
        && chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if !name_ok {
        return invalid("a class name is lower-case letters, digits and underscores, starting with a letter, up to 32");
    }
    if !valid_bps(rate_bps) {
        return invalid("a rate is 0 to 10000 basis points");
    }
    Ok(())
}
