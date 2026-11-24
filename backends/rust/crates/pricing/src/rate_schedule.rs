//! A rate schedule: which [`RateCard`] is in effect when.
//!
//! Providers announce price changes in advance (a rate that doubles on a date). A schedule holds the cards in the
//! order they take effect, so an application states the whole plan once and a call is priced by the card in effect
//! when it **starts**. It is two atoms and no I/O:
//!
//! - [`RateSchedule::card_at`] is the card in effect at an instant.
//! - [`RateSchedule::bound_between`] is what to hold for a call that starts at one instant and may settle at a later
//!   one: the dearest [`RateCard::bound`] of every card in effect in between, so the hold covers a price change in
//!   the middle of a call whichever card ends up pricing it.
//!
//! Times are Unix milliseconds. The schedule does not read a clock and does not parse dates; the caller owns both.
//! A schedule with one step is a single card with a start time.

use num_bigint::BigInt;
use serde::{Deserialize, Serialize};

use crate::{RateCard, RouterError};

/// One card and the instant it takes effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RateStep {
    /// Unix milliseconds from which `card` is in effect, until the next step's `from_ms`.
    pub from_ms: i64,
    pub card: RateCard,
}

/// Cards in the order they take effect. Build one by decoding JSON, or call [`RateSchedule::new`]; both validate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RateSchedule {
    steps: Vec<RateStep>,
}

fn invalid() -> RouterError {
    RouterError::new("invalid_rate_schedule", 400)
}

impl RateSchedule {
    /// Fails with `invalid_rate_schedule` when there is no step, the steps are not in strictly increasing `from_ms`
    /// order, or a card is not a valid [`RateCard`].
    pub fn new(steps: Vec<RateStep>) -> Result<Self, RouterError> {
        let schedule = Self { steps };
        schedule.validate()?;
        Ok(schedule)
    }

    /// Reads a schedule from JSON text, as [`RateSchedule::new`] validates it.
    pub fn from_json(text: &str) -> Result<Self, RouterError> {
        let schedule: Self = serde_json::from_str(text).map_err(|_| invalid())?;
        schedule.validate()?;
        Ok(schedule)
    }

    fn validate(&self) -> Result<(), RouterError> {
        if self.steps.is_empty() || self.steps.windows(2).any(|w| w[0].from_ms >= w[1].from_ms) {
            return Err(invalid());
        }
        self.steps.iter().try_for_each(|s| s.card.effective().map(drop)).map_err(|_| invalid())
    }

    pub fn steps(&self) -> &[RateStep] {
        &self.steps
    }

    /// The card in effect at `at_ms`. Fails with `rate_card_not_in_effect` before the first step: a price that was
    /// never announced is refused, not guessed.
    pub fn card_at(&self, at_ms: i64) -> Result<&RateCard, RouterError> {
        let in_effect = self.steps.partition_point(|s| s.from_ms <= at_ms);
        in_effect.checked_sub(1).map(|i| &self.steps[i].card).ok_or_else(|| RouterError::new("rate_card_not_in_effect", 400))
    }

    /// When the card in effect at `at_ms` is replaced, or `None` if it is the last. For a warning such as "the price
    /// doubles in two days".
    pub fn next_change_after(&self, at_ms: i64) -> Option<i64> {
        self.steps.iter().map(|s| s.from_ms).find(|from| *from > at_ms)
    }

    /// What to hold for a call that starts at `start_ms` and settles by `end_ms`: the dearest [`RateCard::bound`]
    /// of every card in effect at some instant of `start_ms..=end_ms`. [`RateCard::charge`] with the card in effect
    /// at any instant in that range, of counts within the limits, is never more. Fails with `invalid_usage` when
    /// `end_ms` is before `start_ms` or a limit is negative, and with `rate_card_not_in_effect` before the first step.
    pub fn bound_between(&self, start_ms: i64, end_ms: i64, max_input: i64, max_output: i64) -> Result<BigInt, RouterError> {
        if end_ms < start_ms {
            return Err(RouterError::new("invalid_usage", 400));
        }
        let first = self.card_at(start_ms)?;
        let mut dearest = first.bound(max_input, max_output)?;
        for step in self.steps.iter().filter(|s| s.from_ms > start_ms && s.from_ms <= end_ms) {
            dearest = dearest.max(step.card.bound(max_input, max_output)?);
        }
        Ok(dearest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TokenCounts;

    fn card(input: &str, output: &str) -> RateCard {
        RateCard {
            per: 1000,
            input: input.into(),
            cached: None,
            cache_write: None,
            cache_write_1h: None,
            output: output.into(),
            reasoning: None,
            per_call: None,
        }
    }

    /// 1 up to t=1000, doubled from t=1000, halved again from t=5000.
    fn schedule() -> RateSchedule {
        RateSchedule::new(vec![
            RateStep { from_ms: 0, card: card("10", "20") },
            RateStep { from_ms: 1000, card: card("20", "40") },
            RateStep { from_ms: 5000, card: card("5", "10") },
        ])
        .unwrap()
    }

    #[test]
    fn the_card_in_effect_is_the_last_step_that_has_started() {
        let s = schedule();
        assert_eq!(s.card_at(0).unwrap().input, "10");
        assert_eq!(s.card_at(999).unwrap().input, "10");
        assert_eq!(s.card_at(1000).unwrap().input, "20");
        assert_eq!(s.card_at(4999).unwrap().input, "20");
        assert_eq!(s.card_at(5000).unwrap().input, "5");
        assert_eq!(s.card_at(i64::MAX).unwrap().input, "5");
        assert_eq!(s.card_at(-1).unwrap_err().code, "rate_card_not_in_effect");
        assert_eq!(s.next_change_after(0), Some(1000));
        assert_eq!(s.next_change_after(1000), Some(5000));
        assert_eq!(s.next_change_after(5000), None);
    }

    #[test]
    fn a_hold_covers_a_price_change_in_the_middle_of_a_call() {
        let s = schedule();
        let at = |t| s.card_at(t).unwrap().bound(100, 100).unwrap();
        // Entirely before, at and after the doubling: the card in effect.
        assert_eq!(s.bound_between(0, 999, 100, 100).unwrap(), at(0));
        assert_eq!(s.bound_between(1000, 4999, 100, 100).unwrap(), at(1000));
        // Straddling it: the doubled price, even though the call started at the old one.
        assert_eq!(s.bound_between(900, 1100, 100, 100).unwrap(), at(1000));
        // Straddling the later halving: still the dearest card of the range, not the last.
        assert_eq!(s.bound_between(4000, 6000, 100, 100).unwrap(), at(1000));
        assert_eq!(s.bound_between(5000, 6000, 100, 100).unwrap(), at(5000));
        assert!(at(1000) > at(0) && at(0) > at(5000));
    }

    #[test]
    fn whichever_card_prices_the_call_the_hold_is_enough() {
        let s = schedule();
        let counts = TokenCounts { input: 70, output: 30, ..TokenCounts::default() };
        for (start, end) in [(0, 10), (950, 1050), (4990, 5010), (0, 9000), (6000, 7000)] {
            let hold = s.bound_between(start, end, 100, 50).unwrap();
            for t in (start..=end).step_by(5) {
                assert!(s.card_at(t).unwrap().charge(&counts).unwrap() <= hold, "{start}..{end} priced at {t}");
            }
        }
    }

    #[test]
    fn bad_schedules_and_ranges_are_refused() {
        let step = |from_ms, input: &str| RateStep { from_ms, card: card(input, "1") };
        for steps in [vec![], vec![step(5, "1"), step(5, "2")], vec![step(5, "1"), step(4, "2")], vec![step(0, "x")]] {
            assert_eq!(RateSchedule::new(steps).unwrap_err().code, "invalid_rate_schedule");
        }
        assert_eq!(schedule().bound_between(10, 9, 1, 1).unwrap_err().code, "invalid_usage");
        assert_eq!(schedule().bound_between(-5, 0, 1, 1).unwrap_err().code, "rate_card_not_in_effect");
        assert_eq!(schedule().bound_between(0, 1, -1, 1).unwrap_err().code, "invalid_usage");
    }

    #[test]
    fn a_schedule_round_trips_through_json_and_is_validated_on_the_way_in() {
        let text = r#"{"steps":[{"from_ms":0,"card":{"per":1000,"input":"1","output":"2"}},{"from_ms":9,"card":{"per":1000,"input":"2","output":"4"}}]}"#;
        let s = RateSchedule::from_json(text).unwrap();
        assert_eq!(serde_json::to_string(&s).unwrap(), text);
        assert_eq!(RateSchedule::from_json(r#"{"steps":[]}"#).unwrap_err().code, "invalid_rate_schedule");
        assert_eq!(RateSchedule::from_json(r#"{"steps":[],"x":1}"#).unwrap_err().code, "invalid_rate_schedule");
        assert_eq!(RateSchedule::from_json("[").unwrap_err().code, "invalid_rate_schedule");
    }
}
