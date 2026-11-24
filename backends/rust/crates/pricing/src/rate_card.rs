//! A rate card: what each kind of token costs, with the usual fallbacks.
//!
//! [`Price`](crate::Price) is the wire price of an offering and has four token rates. Providers report more kinds
//! of token than that (a cache write that lasts one hour, reasoning tokens) and bill a fixed fee per call. A rate
//! card prices all of them from one small value, so an application can price a call with the same exact big-integer
//! arithmetic the router uses, without adding a price format of its own.
//!
//! A card is three atoms of arithmetic and no I/O:
//!
//! - [`RateCard::effective`] resolves the fallbacks once: a rate that is not given is the rate of the kind it is
//!   priced as. Cached input and cache writes fall back to `input`, a one-hour cache write to `cache_write`,
//!   reasoning to `output`.
//! - [`RateCard::charge`] prices measured [`TokenCounts`].
//! - [`RateCard::bound`] is the amount to hold before the call, for the call's input and output limits.
//!
//! Both money functions use [`RateCard::effective`], so they cannot disagree about a fallback. The cost is the
//! exact sum over every kind, rounded up **once**, plus the per-call fee:
//!
//! ```text
//! atoms = ceil( (Σ count_k × rate_k) / per ) + per_call
//! ```
//!
//! This module does not change the wire price, the contracts or any store.

use num_bigint::BigInt;
use num_traits::{Signed, Zero};
use serde::{Deserialize, Serialize};

use crate::{atoms, ceil_div, valid_atoms, RouterError};

/// The most tokens one rate may cover, the same limit as [`Price::per`](crate::Price).
const MAX_PER: i64 = 1_000_000_000;

/// What each kind of token costs. Rates are atoms (decimal strings) per `per` tokens.
///
/// `input`, `output` and `per` are required. A kind left out is priced as the kind it falls back to; a rate given
/// replaces the fallback, even when it is `"0"`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RateCard {
    /// Tokens one rate covers (atoms per `per` tokens), at least 1. Rates per million tokens use `1000000`.
    pub per: i64,
    /// Input tokens that were neither read from nor written to a cache.
    pub input: String,
    /// Input tokens read from a cache. Default: `input`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached: Option<String>,
    /// Input tokens written to the cache for the default lifetime (five minutes). Default: `input`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write: Option<String>,
    /// Input tokens written to the cache for one hour. Default: `cache_write` (as resolved).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_1h: Option<String>,
    /// Visible output tokens.
    pub output: String,
    /// Reasoning tokens the provider reports apart from the output. Default: `output`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    /// A fixed fee in atoms added once to every call, however few tokens it used. Default: none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_call: Option<String>,
}

/// Measured token counts for one call. Every token is in exactly one kind, so the kinds add up to the call's total.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenCounts {
    #[serde(default)]
    pub input: i64,
    #[serde(default)]
    pub cached: i64,
    #[serde(default)]
    pub cache_write: i64,
    #[serde(default)]
    pub cache_write_1h: i64,
    #[serde(default)]
    pub output: i64,
    #[serde(default)]
    pub reasoning: i64,
}

/// The six kinds of token a card prices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TokenKind {
    Input,
    Cached,
    CacheWrite,
    CacheWrite1h,
    Output,
    Reasoning,
}

impl TokenKind {
    /// The field name, as in [`RateCard`] and [`TokenCounts`].
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Input => "input",
            Self::Cached => "cached",
            Self::CacheWrite => "cache_write",
            Self::CacheWrite1h => "cache_write_1h",
            Self::Output => "output",
            Self::Reasoning => "reasoning",
        }
    }
}

/// The rate of every kind after the fallbacks, in atoms per [`EffectiveRates::per`] tokens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveRates {
    pub per: BigInt,
    pub input: BigInt,
    pub cached: BigInt,
    pub cache_write: BigInt,
    pub cache_write_1h: BigInt,
    pub output: BigInt,
    pub reasoning: BigInt,
    pub per_call: BigInt,
}

fn invalid_card() -> RouterError {
    RouterError::new("invalid_rate_card", 400)
}

fn invalid_counts() -> RouterError {
    RouterError::new("invalid_usage", 400)
}

fn rate(text: &str) -> Result<BigInt, RouterError> {
    if valid_atoms(text) {
        atoms(text).map_err(|_| invalid_card())
    } else {
        Err(invalid_card())
    }
}

fn or(given: &Option<String>, fallback: &BigInt) -> Result<BigInt, RouterError> {
    given.as_deref().map_or_else(|| Ok(fallback.clone()), rate)
}

impl RateCard {
    /// Resolves the fallbacks. Fails with `invalid_rate_card` when `per` is outside `1..=1_000_000_000` or a rate is
    /// not a decimal atom amount (`^(0|[1-9][0-9]{0,29})$`).
    pub fn effective(&self) -> Result<EffectiveRates, RouterError> {
        if !(1..=MAX_PER).contains(&self.per) {
            return Err(invalid_card());
        }
        let input = rate(&self.input)?;
        let output = rate(&self.output)?;
        let cached = or(&self.cached, &input)?;
        let cache_write = or(&self.cache_write, &input)?;
        let cache_write_1h = or(&self.cache_write_1h, &cache_write)?;
        let reasoning = or(&self.reasoning, &output)?;
        let per_call = or(&self.per_call, &BigInt::zero())?;
        Ok(EffectiveRates { per: BigInt::from(self.per), input, cached, cache_write, cache_write_1h, output, reasoning, per_call })
    }

    /// The kinds whose rate the card does not give, so that they are priced by fallback. `input` and `output` are
    /// required and never appear. A fallback is often the right price (a provider that charges cache reads like input)
    /// and sometimes a forgotten one (a cache read billed at the full input rate), and the card cannot tell which;
    /// this lets an application ask. It changes no price.
    pub fn defaulted(&self) -> Vec<TokenKind> {
        [
            (TokenKind::Cached, &self.cached),
            (TokenKind::CacheWrite, &self.cache_write),
            (TokenKind::CacheWrite1h, &self.cache_write_1h),
            (TokenKind::Reasoning, &self.reasoning),
        ]
        .into_iter()
        .filter_map(|(kind, rate)| rate.is_none().then_some(kind))
        .collect()
    }

    /// The kinds that `counts` used and that are priced by fallback: the ones worth a look when a bill seems high.
    /// A call that read cached tokens from a card with no `cached` rate is `[Cached]`.
    pub fn defaulted_in(&self, counts: &TokenCounts) -> Vec<TokenKind> {
        let used = |kind| match kind {
            TokenKind::Cached => counts.cached,
            TokenKind::CacheWrite => counts.cache_write,
            TokenKind::CacheWrite1h => counts.cache_write_1h,
            TokenKind::Reasoning => counts.reasoning,
            TokenKind::Input => counts.input,
            TokenKind::Output => counts.output,
        } > 0;
        self.defaulted().into_iter().filter(|kind| used(*kind)).collect()
    }

    /// The price of a call that used `counts`. Fails with `invalid_usage` when a count is negative.
    pub fn charge(&self, counts: &TokenCounts) -> Result<BigInt, RouterError> {
        let r = self.effective()?;
        let numerator = [
            (counts.input, &r.input),
            (counts.cached, &r.cached),
            (counts.cache_write, &r.cache_write),
            (counts.cache_write_1h, &r.cache_write_1h),
            (counts.output, &r.output),
            (counts.reasoning, &r.reasoning),
        ]
        .into_iter()
        .try_fold(BigInt::zero(), |sum, (n, rate)| if n < 0 { Err(invalid_counts()) } else { Ok(sum + BigInt::from(n) * rate) })?;
        Ok(ceil_div(&numerator, &r.per) + r.per_call)
    }

    /// The most a call can cost when its input kinds add up to at most `max_input` tokens and its output and
    /// reasoning tokens to at most `max_output`: the dearest input rate times `max_input`, the dearer of output and
    /// reasoning times `max_output`, and the per-call fee. Hold this before the call; [`RateCard::charge`] of any
    /// counts within those limits is never more. Reasoning tokens count toward `max_output`, as providers count
    /// them toward their own output limit. Fails with `invalid_usage` when a limit is negative.
    pub fn bound(&self, max_input: i64, max_output: i64) -> Result<BigInt, RouterError> {
        if max_input < 0 || max_output < 0 {
            return Err(invalid_counts());
        }
        let r = self.effective()?;
        let dearest_input = [&r.input, &r.cached, &r.cache_write, &r.cache_write_1h].into_iter().max().expect("four rates").clone();
        let dearest_output = (&r.output).max(&r.reasoning).clone();
        let numerator = BigInt::from(max_input) * dearest_input + BigInt::from(max_output) * dearest_output;
        debug_assert!(!numerator.is_negative());
        Ok(ceil_div(&numerator, &r.per) + r.per_call)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{bound, charge, Price, Usage};

    fn card(per: i64, input: &str, output: &str) -> RateCard {
        RateCard {
            per,
            input: input.into(),
            cached: None,
            cache_write: None,
            cache_write_1h: None,
            output: output.into(),
            reasoning: None,
            per_call: None,
        }
    }

    /// The same four rates as a wire price: a card with no extra kind and no fee is exactly `charge` and `bound`.
    #[test]
    fn a_card_without_extras_is_the_wire_price() {
        let c = RateCard { cached: Some("3".into()), cache_write: Some("9".into()), ..card(7, "5", "11") };
        let price = Price {
            units: Default::default(),
            asset: "credits".into(),
            input: "5".into(),
            cached: "3".into(),
            cache_write: "9".into(),
            output: "11".into(),
            per: 7,
            media: Default::default(),
        };
        let mut seed = 20261124_u64;
        let mut next = |n: i64| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((seed >> 33) % n as u64) as i64
        };
        for _ in 0..500 {
            let counts = TokenCounts {
                input: next(5000),
                cached: next(5000),
                cache_write: next(5000),
                output: next(5000),
                ..TokenCounts::default()
            };
            let usage = Usage {
                input: counts.input,
                cached: counts.cached,
                cache_write: counts.cache_write,
                output: counts.output,
                ..Usage::default()
            };
            assert_eq!(c.charge(&counts).unwrap(), charge(&usage, &price).unwrap());
            let (i, o) = (next(100000), next(100000));
            assert_eq!(c.bound(i, o).unwrap(), bound(i, o, &price).unwrap());
        }
    }

    /// Whatever the counts, a call within its limits never costs more than was held for it.
    #[test]
    fn a_charge_within_the_limits_never_exceeds_the_bound() {
        let c = RateCard {
            cached: Some("2".into()),
            cache_write: Some("40".into()),
            cache_write_1h: Some("90".into()),
            reasoning: Some("120".into()),
            per_call: Some("13".into()),
            ..card(1000, "30", "100")
        };
        let mut seed = 7_u64;
        let mut next = |n: i64| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((seed >> 33) % n as u64) as i64
        };
        for _ in 0..1000 {
            let (max_in, max_out) = (next(20000), next(20000));
            // split each limit across the kinds that share it
            let (a, b, d) = (next(max_in + 1), next(max_in + 1), next(max_in + 1));
            let mut parts = [a, b, d];
            parts.sort_unstable();
            let input = parts[0];
            let cached = parts[1] - parts[0];
            let cache_write = parts[2] - parts[1];
            let cache_write_1h = max_in - parts[2];
            let reasoning = next(max_out + 1);
            let counts = TokenCounts { input, cached, cache_write, cache_write_1h, output: max_out - reasoning, reasoning };
            assert!(c.charge(&counts).unwrap() <= c.bound(max_in, max_out).unwrap(), "{counts:?} within {max_in}/{max_out}");
        }
    }

    #[test]
    fn effective_rates_resolve_the_fallbacks_in_order() {
        let e = card(1, "5", "11").effective().unwrap();
        assert_eq!([&e.cached, &e.cache_write, &e.cache_write_1h].map(|r| r.to_string()), ["5", "5", "5"]);
        assert_eq!(e.reasoning.to_string(), "11");
        let e = RateCard { cache_write: Some("8".into()), ..card(1, "5", "11") }.effective().unwrap();
        assert_eq!(e.cache_write_1h.to_string(), "8");
        // a rate of zero is a rate, not a missing one
        let e = RateCard { cached: Some("0".into()), ..card(1, "5", "11") }.effective().unwrap();
        assert_eq!(e.cached.to_string(), "0");
    }

    #[test]
    fn defaulted_names_the_kinds_priced_by_fallback() {
        assert_eq!(
            card(1, "5", "11").defaulted(),
            [TokenKind::Cached, TokenKind::CacheWrite, TokenKind::CacheWrite1h, TokenKind::Reasoning]
        );
        // A rate of zero is a rate, so it is not defaulted.
        let c = RateCard { cached: Some("0".into()), cache_write: Some("8".into()), ..card(1, "5", "11") };
        assert_eq!(c.defaulted(), [TokenKind::CacheWrite1h, TokenKind::Reasoning]);
        let counts = TokenCounts { input: 9, cached: 4, output: 2, ..TokenCounts::default() };
        assert_eq!(card(1, "5", "11").defaulted_in(&counts), [TokenKind::Cached]);
        assert_eq!(c.defaulted_in(&counts), []);
        // Required kinds are never reported, whatever the counts.
        assert_eq!(card(1, "5", "11").defaulted_in(&TokenCounts { input: 1, output: 1, ..TokenCounts::default() }), []);
    }

    #[test]
    fn a_card_round_trips_through_json_and_refuses_unknown_keys() {
        let text = r#"{"per":1000000,"input":"1","output":"2","reasoning":"3","per_call":"4"}"#;
        let c: RateCard = serde_json::from_str(text).unwrap();
        assert_eq!(serde_json::to_string(&c).unwrap(), text);
        assert!(serde_json::from_str::<RateCard>(r#"{"per":1,"input":"1","output":"1","cache":"1"}"#).is_err());
        assert!(serde_json::from_str::<TokenCounts>(r#"{"output":1,"thinking":2}"#).is_err());
    }
}
