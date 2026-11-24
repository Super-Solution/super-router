//! Long-context price tiers: which [`RateCard`] prices a call by how long its prompt was.
//!
//! Some models cost more for the whole call once the prompt passes a size (Gemini 2.5 Pro and 3 Pro: every rate
//! rises when the prompt is over 200,000 tokens, the output and the cache included, not only the tokens past the
//! line). Tiers state that once. It is two atoms and no I/O:
//!
//! - [`RateTiers::card_for`] is the card for a prompt of `n` tokens: the last tier whose `above` is less than `n`.
//! - [`RateTiers::bound`] is what to hold before the call, when only the largest prompt is known: the dearest
//!   [`RateCard::bound`] over every tier a prompt of up to `max_input` tokens can reach, so the hold covers the call
//!   whichever tier ends up pricing it.
//!
//! The prompt of a call is every input kind added together (`input`, `cached`, `cache_write`, `cache_write_1h`),
//! which is also what [`RateCard::bound`] caps with `max_input`. Tiers by date are a [`crate::RateSchedule`]; to
//! have both, keep one `RateTiers` per step.

use num_bigint::BigInt;
use serde::{Deserialize, Serialize};

use crate::{RateCard, RouterError, TokenCounts};

/// One card and the prompt size above which it prices the call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RateTier {
    /// The card applies when the prompt is **more than** this many tokens (a prompt of exactly this size is priced by
    /// the tier below). The first tier's `above` is 0.
    pub above: i64,
    pub card: RateCard,
}

/// Cards by prompt size. Build one by decoding JSON, or call [`RateTiers::new`]; both validate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RateTiers {
    tiers: Vec<RateTier>,
}

fn invalid() -> RouterError {
    RouterError::new("invalid_rate_tiers", 400)
}

impl RateTiers {
    /// Fails with `invalid_rate_tiers` when there is no tier, the first `above` is not 0, the `above` values are not
    /// strictly increasing, or a card is not a valid [`RateCard`].
    pub fn new(tiers: Vec<RateTier>) -> Result<Self, RouterError> {
        let tiers = Self { tiers };
        tiers.validate()?;
        Ok(tiers)
    }

    /// Reads tiers from JSON text, as [`RateTiers::new`] validates them.
    pub fn from_json(text: &str) -> Result<Self, RouterError> {
        let tiers: Self = serde_json::from_str(text).map_err(|_| invalid())?;
        tiers.validate()?;
        Ok(tiers)
    }

    fn validate(&self) -> Result<(), RouterError> {
        let ordered = self.tiers.first().is_some_and(|t| t.above == 0) && self.tiers.windows(2).all(|w| w[0].above < w[1].above);
        if !ordered {
            return Err(invalid());
        }
        self.tiers.iter().try_for_each(|t| t.card.effective().map(drop)).map_err(|_| invalid())
    }

    pub fn tiers(&self) -> &[RateTier] {
        &self.tiers
    }

    /// The card that prices a call whose prompt is `prompt_tokens` long. Fails with `invalid_usage` when it is
    /// negative.
    pub fn card_for(&self, prompt_tokens: i64) -> Result<&RateCard, RouterError> {
        if prompt_tokens < 0 {
            return Err(RouterError::new("invalid_usage", 400));
        }
        let reached = self.tiers.partition_point(|t| t.above < prompt_tokens);
        Ok(&self.tiers[reached.saturating_sub(1)].card)
    }

    /// The price of a call that used `counts`, by the card its prompt size selects. Fails with `invalid_usage` when
    /// a count is negative.
    pub fn charge(&self, counts: &TokenCounts) -> Result<BigInt, RouterError> {
        if [counts.input, counts.cached, counts.cache_write, counts.cache_write_1h].iter().any(|n| *n < 0) {
            return Err(RouterError::new("invalid_usage", 400));
        }
        // Four non-negative i64 never overflow an i128.
        let prompt =
            i128::from(counts.input) + i128::from(counts.cached) + i128::from(counts.cache_write) + i128::from(counts.cache_write_1h);
        let prompt = i64::try_from(prompt).unwrap_or(i64::MAX);
        self.card_for(prompt)?.charge(counts)
    }

    /// What to hold for a call whose input kinds add up to at most `max_input` tokens and whose output and reasoning
    /// add up to at most `max_output`: the dearest [`RateCard::bound`] of every tier a prompt of up to `max_input`
    /// can reach. [`RateTiers::charge`] of any counts within those limits is never more. Fails with `invalid_usage`
    /// when a limit is negative.
    pub fn bound(&self, max_input: i64, max_output: i64) -> Result<BigInt, RouterError> {
        let mut dearest = self.tiers[0].card.bound(max_input, max_output)?;
        for tier in self.tiers.iter().skip(1).filter(|t| t.above < max_input) {
            dearest = dearest.max(tier.card.bound(max_input, max_output)?);
        }
        Ok(dearest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(input: &str, output: &str) -> RateCard {
        RateCard {
            per: 1_000_000,
            input: input.into(),
            cached: None,
            cache_write: None,
            cache_write_1h: None,
            output: output.into(),
            reasoning: None,
            per_call: None,
        }
    }

    /// Up to 200,000 prompt tokens: 1.25 in, 10 out. Over 200,000: 2.5 in, 15 out. Over 1,000,000: 5 in, 30 out.
    fn tiers() -> RateTiers {
        RateTiers::new(vec![
            RateTier { above: 0, card: card("1250000", "10000000") },
            RateTier { above: 200_000, card: card("2500000", "15000000") },
            RateTier { above: 1_000_000, card: card("5000000", "30000000") },
        ])
        .unwrap()
    }

    #[test]
    fn the_whole_call_is_priced_by_the_tier_its_prompt_reaches() {
        let t = tiers();
        assert_eq!(t.card_for(0).unwrap().input, "1250000");
        assert_eq!(t.card_for(200_000).unwrap().input, "1250000", "exactly on the line is the lower tier");
        assert_eq!(t.card_for(200_001).unwrap().input, "2500000");
        assert_eq!(t.card_for(1_000_000).unwrap().input, "2500000");
        assert_eq!(t.card_for(1_000_001).unwrap().input, "5000000");
        assert_eq!(t.card_for(i64::MAX).unwrap().input, "5000000");
        assert_eq!(t.card_for(-1).unwrap_err().code, "invalid_usage");
        // 150k in + 50k cached + 10k out: a 200k prompt, lower tier. One more token tips the whole call.
        let low = TokenCounts { input: 150_000, cached: 50_000, output: 10_000, ..TokenCounts::default() };
        let high = TokenCounts { input: 150_001, ..low };
        assert_eq!(t.charge(&low).unwrap(), BigInt::from(150_000 * 125 / 100 + 50_000 * 125 / 100 + 10_000 * 10));
        // 200,001 prompt tokens at 2.5 plus 10,000 output at 15 is 650,002.5, rounded up once.
        assert_eq!(t.charge(&high).unwrap(), BigInt::from(650_003));
    }

    #[test]
    fn a_hold_covers_every_tier_the_prompt_can_reach() {
        let t = tiers();
        let at = |i: usize| t.tiers()[i].card.bound(500_000, 8_000).unwrap();
        assert_eq!(t.bound(150_000, 8_000).unwrap(), t.tiers()[0].card.bound(150_000, 8_000).unwrap());
        assert_eq!(t.bound(200_000, 8_000).unwrap(), t.tiers()[0].card.bound(200_000, 8_000).unwrap(), "200k cannot reach the next tier");
        assert_eq!(t.bound(500_000, 8_000).unwrap(), at(1));
        assert_eq!(t.bound(2_000_000, 8_000).unwrap(), t.tiers()[2].card.bound(2_000_000, 8_000).unwrap());
        assert_eq!(t.bound(-1, 0).unwrap_err().code, "invalid_usage");
        for max_input in [0_i64, 1, 199_999, 200_000, 200_001, 999_999, 1_000_000, 1_000_001, 3_000_000] {
            let hold = t.bound(max_input, 50).unwrap();
            for prompt in [0, 1, max_input / 3, max_input - 1, max_input].into_iter().filter(|p| (0..=max_input).contains(p)) {
                let counts = TokenCounts { input: prompt / 2, cached: prompt - prompt / 2, output: 50, ..TokenCounts::default() };
                assert!(t.charge(&counts).unwrap() <= hold, "max_input {max_input}, prompt {prompt}");
            }
        }
    }

    #[test]
    fn bad_tiers_are_refused() {
        let tier = |above, input: &str| RateTier { above, card: card(input, "1") };
        for list in [
            vec![],
            vec![tier(1, "1")],
            vec![tier(0, "1"), tier(0, "2")],
            vec![tier(0, "1"), tier(9, "2"), tier(5, "3")],
            vec![tier(0, "x")],
        ] {
            assert_eq!(RateTiers::new(list).unwrap_err().code, "invalid_rate_tiers");
        }
        let text = r#"{"tiers":[{"above":0,"card":{"per":1000,"input":"1","output":"2"}},{"above":9,"card":{"per":1000,"input":"2","output":"4"}}]}"#;
        assert_eq!(serde_json::to_string(&RateTiers::from_json(text).unwrap()).unwrap(), text);
        for bad in [r#"{"tiers":[]}"#, r#"{"tiers":[],"x":1}"#, "["] {
            assert_eq!(RateTiers::from_json(bad).unwrap_err().code, "invalid_rate_tiers");
        }
        assert_eq!(tiers().charge(&TokenCounts { cached: -1, ..TokenCounts::default() }).unwrap_err().code, "invalid_usage");
    }
}
