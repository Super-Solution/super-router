// SPDX-License-Identifier: Apache-2.0

package pricing

import (
	"bytes"
	"encoding/json"
	"math"
	"math/big"
	"sort"
)

// RateTier is one card and the prompt size above which it prices the call.
type RateTier struct {
	// Above: the card applies when the prompt is more than this many tokens (a prompt of exactly this size is priced
	// by the tier below). The first tier's Above is 0.
	Above int64    `json:"above"`
	Card  RateCard `json:"card"`
}

// RateTiers is long-context pricing: which RateCard prices a call by how long its prompt was.
//
// Some models cost more for the whole call once the prompt passes a size (Gemini 2.5 Pro and 3 Pro: every rate rises
// when the prompt is over 200,000 tokens, the output and the cache included, not only the tokens past the line).
// Tiers state that once. It is two atoms and no I/O: CardFor is the card for a prompt of n tokens (the last tier
// whose Above is less than n), and Bound is what to hold before the call, when only the largest prompt is known: the
// dearest RateCard.Bound over every tier a prompt of up to maxInput tokens can reach.
//
// The prompt of a call is every input kind added together (Input, Cached, CacheWrite, CacheWrite1h), which is also
// what RateCard.Bound caps with maxInput. Tiers by date are a RateSchedule; to have both, keep one RateTiers per step.
// Build one with NewRateTiers or DecodeRateTiers; both validate.
type RateTiers struct {
	tiers []RateTier
}

type tiersJSON struct {
	Tiers []RateTier `json:"tiers"`
}

func invalidTiers() *RouterError { return fail("invalid_rate_tiers", 400) }

// NewRateTiers fails with invalid_rate_tiers when there is no tier, the first Above is not 0, the Above values are
// not strictly increasing, or a card is not a valid RateCard.
func NewRateTiers(tiers []RateTier) (RateTiers, error) {
	if len(tiers) == 0 || tiers[0].Above != 0 {
		return RateTiers{}, invalidTiers()
	}
	for i, t := range tiers {
		if i > 0 && tiers[i-1].Above >= t.Above {
			return RateTiers{}, invalidTiers()
		}
		if _, err := t.Card.Effective(); err != nil {
			return RateTiers{}, invalidTiers()
		}
	}
	return RateTiers{tiers: append([]RateTier(nil), tiers...)}, nil
}

// DecodeRateTiers parses tiers, refuses keys it does not know and validates them, with invalid_rate_tiers.
func DecodeRateTiers(data []byte) (RateTiers, error) {
	var raw tiersJSON
	d := json.NewDecoder(bytes.NewReader(data))
	d.DisallowUnknownFields()
	if err := d.Decode(&raw); err != nil {
		return RateTiers{}, invalidTiers()
	}
	return NewRateTiers(raw.Tiers)
}

// MarshalJSON writes the tiers in the form DecodeRateTiers reads.
func (r RateTiers) MarshalJSON() ([]byte, error) { return json.Marshal(tiersJSON{Tiers: r.tiers}) }

// Tiers returns a copy of the tiers.
func (r RateTiers) Tiers() []RateTier { return append([]RateTier(nil), r.tiers...) }

// CardFor is the card that prices a call whose prompt is promptTokens long. It fails with invalid_usage when that is
// negative.
func (r RateTiers) CardFor(promptTokens int64) (RateCard, error) {
	if promptTokens < 0 {
		return RateCard{}, invalidCounts()
	}
	reached := sort.Search(len(r.tiers), func(i int) bool { return r.tiers[i].Above >= promptTokens })
	if reached > 0 {
		reached--
	}
	return r.tiers[reached].Card, nil
}

// Charge is the price of a call that used counts, by the card its prompt size selects. It fails with invalid_usage
// when a count is negative.
func (r RateTiers) Charge(counts TokenCounts) (*big.Int, error) {
	prompt := new(big.Int)
	for _, n := range []int64{counts.Input, counts.Cached, counts.CacheWrite, counts.CacheWrite1h} {
		if n < 0 {
			return nil, invalidCounts()
		}
		prompt.Add(prompt, big.NewInt(n))
	}
	size := int64(math.MaxInt64)
	if prompt.IsInt64() {
		size = prompt.Int64()
	}
	card, err := r.CardFor(size)
	if err != nil {
		return nil, err
	}
	return card.Charge(counts)
}

// Bound is what to hold for a call whose input kinds add up to at most maxInput tokens and whose output and reasoning
// add up to at most maxOutput: the dearest RateCard.Bound of every tier a prompt of up to maxInput can reach.
// Charge of any counts within those limits is never more. It fails with invalid_usage when a limit is negative.
func (r RateTiers) Bound(maxInput, maxOutput int64) (*big.Int, error) {
	dearest, err := r.tiers[0].Card.Bound(maxInput, maxOutput)
	if err != nil {
		return nil, err
	}
	for _, t := range r.tiers[1:] {
		if t.Above >= maxInput {
			continue
		}
		hold, err := t.Card.Bound(maxInput, maxOutput)
		if err != nil {
			return nil, err
		}
		if hold.Cmp(dearest) > 0 {
			dearest = hold
		}
	}
	return dearest, nil
}
