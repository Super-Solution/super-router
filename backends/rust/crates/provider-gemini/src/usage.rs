//! `usageMetadata` as router meters.
//!
//! Gemini counts differently from the other dialects in three ways, all handled here and nowhere else:
//!
//! - `promptTokenCount` **includes** the cached tokens (`cachedContentTokenCount`), so the uncached input is the
//!   difference. Tool-use prompt tokens (`toolUsePromptTokenCount`, the results of built-in tools) are billed as
//!   input and added to it.
//! - `candidatesTokenCount` is normally the visible answer only; thinking is reported apart in `thoughtsTokenCount`
//!   and billed at the output rate, so the two add up to the output meter. Some responses (seen on Vertex) fold the
//!   thoughts into `candidatesTokenCount`; they are told apart by the total, see below.
//! - There is no cache-write count: implicit caching writes for free and explicit caches bill storage by the hour,
//!   which is not a token meter. `cache_write` stays zero.
//!
//! `totalTokenCount` is a cross-check, as it is for FirstFa's own normaliser: tokens the total counts that no
//! field accounts for are priced as output, so nothing the provider counted goes unpriced.

use serde::{Deserialize, Deserializer};
use super_router_contracts::Meters;

/// Null reads as the default, the way a missing field does.
fn lenient<'de, D: Deserializer<'de>>(d: D) -> Result<Option<i64>, D::Error> {
    Option::<i64>::deserialize(d)
}

/// The counters of a chunk's `usageMetadata`. Only the prompt count is required: a blocked or empty answer has no
/// candidates count.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageMetadata {
    #[serde(default, deserialize_with = "lenient")]
    pub prompt_token_count: Option<i64>,
    #[serde(default, deserialize_with = "lenient")]
    pub cached_content_token_count: Option<i64>,
    #[serde(default, deserialize_with = "lenient")]
    pub tool_use_prompt_token_count: Option<i64>,
    #[serde(default, deserialize_with = "lenient")]
    pub candidates_token_count: Option<i64>,
    #[serde(default, deserialize_with = "lenient")]
    pub thoughts_token_count: Option<i64>,
    #[serde(default, deserialize_with = "lenient")]
    pub total_token_count: Option<i64>,
}

/// Router meters for one call, or `None` when the usage cannot be trusted: no prompt count, a negative count, more
/// cached tokens than prompt tokens, or a sum that does not fit.
pub fn normalize(u: &UsageMetadata) -> Option<Meters> {
    let prompt = u.prompt_token_count?;
    let cached = u.cached_content_token_count.unwrap_or(0);
    let tool_use = u.tool_use_prompt_token_count.unwrap_or(0);
    let candidates = u.candidates_token_count.unwrap_or(0);
    let thoughts = u.thoughts_token_count.unwrap_or(0);
    if [prompt, cached, tool_use, candidates, thoughts].iter().any(|n| *n < 0) {
        return None;
    }
    let input = prompt.checked_sub(cached)?.checked_add(tool_use)?;
    // The total is prompt + candidates + tool use + thoughts. When it equals the same sum without the thoughts, the
    // thoughts were already inside `candidatesTokenCount`: adding them again would bill them twice.
    let folded = thoughts > 0
        && u.total_token_count.is_some_and(|total| prompt.checked_add(candidates).and_then(|n| n.checked_add(tool_use)) == Some(total));
    let mut output = if folded { candidates } else { candidates.checked_add(thoughts)? };
    if let Some(total) = u.total_token_count {
        let counted = input.checked_add(cached)?.checked_add(output)?;
        if total > counted {
            output = output.checked_add(total - counted)?;
        }
    }
    let meters = Meters { input, cached, output, ..Meters::default() };
    meters.is_valid().then_some(meters)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(prompt: i64, cached: i64, tool: i64, candidates: i64, thoughts: i64, total: Option<i64>) -> UsageMetadata {
        UsageMetadata {
            prompt_token_count: Some(prompt),
            cached_content_token_count: Some(cached),
            tool_use_prompt_token_count: Some(tool),
            candidates_token_count: Some(candidates),
            thoughts_token_count: Some(thoughts),
            total_token_count: total,
        }
    }

    #[test]
    fn cached_tokens_come_out_of_the_prompt_and_thoughts_join_the_output() {
        let m = normalize(&usage(1000, 400, 0, 50, 30, Some(1080))).unwrap();
        assert_eq!((m.input, m.cached, m.cache_write, m.output), (600, 400, 0, 80));
    }

    #[test]
    fn thoughts_already_inside_the_candidates_are_not_added_twice() {
        // prompt 100 + candidates 40 (30 of them thoughts) = total 140: the thoughts are folded in.
        let m = normalize(&usage(100, 0, 0, 40, 30, Some(140))).unwrap();
        assert_eq!((m.input, m.output), (100, 40));
        // The same fields with the total that counts them apart: they add up.
        let m = normalize(&usage(100, 0, 0, 40, 30, Some(170))).unwrap();
        assert_eq!((m.input, m.output), (100, 70));
        // Tool-use tokens are part of the comparison.
        let m = normalize(&usage(100, 20, 5, 40, 30, Some(145))).unwrap();
        assert_eq!((m.input, m.cached, m.output), (85, 20, 40));
        // No total, no way to tell: the documented reading, thoughts apart.
        let m = normalize(&usage(100, 0, 0, 40, 30, None)).unwrap();
        assert_eq!(m.output, 70);
    }

    #[test]
    fn tool_use_prompt_tokens_are_input() {
        let m = normalize(&usage(100, 0, 25, 10, 0, Some(135))).unwrap();
        assert_eq!((m.input, m.output), (125, 10));
    }

    #[test]
    fn a_total_larger_than_the_fields_adds_the_rest_to_output() {
        let m = normalize(&usage(100, 0, 0, 10, 0, Some(150))).unwrap();
        assert_eq!((m.input, m.output), (100, 50));
        // A smaller total changes nothing: nothing itemised goes unpriced.
        let m = normalize(&usage(100, 0, 0, 10, 0, Some(50))).unwrap();
        assert_eq!((m.input, m.output), (100, 10));
    }

    #[test]
    fn only_the_prompt_count_is_required() {
        let blocked = UsageMetadata { prompt_token_count: Some(7), total_token_count: Some(7), ..UsageMetadata::default() };
        let m = normalize(&blocked).unwrap();
        assert_eq!((m.input, m.output), (7, 0));
        assert_eq!(normalize(&UsageMetadata::default()), None);
    }

    #[test]
    fn untrustworthy_usage_is_refused() {
        assert_eq!(normalize(&usage(10, 11, 0, 0, 0, None)), None, "more cached than prompt");
        assert_eq!(normalize(&usage(-1, 0, 0, 0, 0, None)), None);
        assert_eq!(normalize(&usage(10, 0, 0, -1, 0, None)), None);
        assert_eq!(normalize(&usage(i64::MAX, 0, 1, 0, 0, None)), None, "overflow");
        assert_eq!(normalize(&usage(1, 0, 0, i64::MAX, 1, None)), None, "overflow");
    }

    #[test]
    fn null_counts_read_as_absent() {
        let u: UsageMetadata =
            serde_json::from_str(r#"{"promptTokenCount":5,"cachedContentTokenCount":null,"candidatesTokenCount":2}"#).unwrap();
        let m = normalize(&u).unwrap();
        assert_eq!((m.input, m.cached, m.output), (5, 0, 2));
    }
}
