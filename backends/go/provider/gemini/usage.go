// SPDX-License-Identifier: Apache-2.0

package gemini

import (
	"math"

	"github.com/Super-Solution/super-router/backends/go/contracts"
)

// UsageMetadata is a chunk's `usageMetadata`. Counts are pointers: only the prompt count is required (a blocked or
// empty answer has no candidates count), and null reads as absent.
type UsageMetadata struct {
	PromptTokenCount        *int64 `json:"promptTokenCount"`
	CachedContentTokenCount *int64 `json:"cachedContentTokenCount"`
	ToolUsePromptTokenCount *int64 `json:"toolUsePromptTokenCount"`
	CandidatesTokenCount    *int64 `json:"candidatesTokenCount"`
	ThoughtsTokenCount      *int64 `json:"thoughtsTokenCount"`
	TotalTokenCount         *int64 `json:"totalTokenCount"`
}

func or0(n *int64) int64 {
	if n == nil {
		return 0
	}
	return *n
}

// add returns a+b for non-negative a and b, and false when the sum does not fit.
func add(a, b int64) (int64, bool) {
	if a > math.MaxInt64-b {
		return 0, false
	}
	return a + b, true
}

// Normalize turns usage into router meters, or reports false when it cannot be trusted: no prompt count, a negative
// count, more cached tokens than prompt tokens, or a sum that does not fit.
//
// Gemini counts the cached tokens inside promptTokenCount, so the uncached input is the difference; tool-use prompt
// tokens are billed as input and added to it. candidatesTokenCount is normally the visible answer only and
// thinking is reported apart in thoughtsTokenCount, billed at the output rate, so the two add up to the output meter;
// when the total shows the thoughts are already inside candidatesTokenCount they are not added again. There is
// no cache-write count (implicit caching writes for free, explicit caches bill storage by the hour). Tokens that only
// totalTokenCount counts are priced as output, so nothing the provider counted goes unpriced.
func Normalize(u UsageMetadata) (contracts.Meters, bool) {
	if u.PromptTokenCount == nil {
		return contracts.Meters{}, false
	}
	prompt, cached, toolUse, candidates, thoughts := *u.PromptTokenCount, or0(u.CachedContentTokenCount), or0(u.ToolUsePromptTokenCount), or0(u.CandidatesTokenCount), or0(u.ThoughtsTokenCount)
	for _, n := range []int64{prompt, cached, toolUse, candidates, thoughts} {
		if n < 0 {
			return contracts.Meters{}, false
		}
	}
	if cached > prompt {
		return contracts.Meters{}, false
	}
	input, ok := add(prompt-cached, toolUse)
	if !ok {
		return contracts.Meters{}, false
	}
	// The total is prompt + candidates + tool use + thoughts. When it equals the same sum without the thoughts, the
	// thoughts were already inside candidatesTokenCount (seen on Vertex): adding them again would bill them twice.
	folded := false
	if u.TotalTokenCount != nil && thoughts > 0 {
		if sum, ok := add(prompt, candidates); ok {
			if sum, ok = add(sum, toolUse); ok && sum == *u.TotalTokenCount {
				folded = true
			}
		}
	}
	output, ok := candidates, true
	if !folded {
		output, ok = add(candidates, thoughts)
	}
	if !ok {
		return contracts.Meters{}, false
	}
	if u.TotalTokenCount != nil {
		counted, ok1 := add(input, cached)
		counted, ok2 := add(counted, output)
		if !ok1 || !ok2 {
			return contracts.Meters{}, false
		}
		if *u.TotalTokenCount > counted {
			if output, ok = add(output, *u.TotalTokenCount-counted); !ok {
				return contracts.Meters{}, false
			}
		}
	}
	m := contracts.Meters{Input: input, Cached: cached, Output: output}
	return m, m.Valid()
}
