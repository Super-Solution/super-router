// SPDX-License-Identifier: Apache-2.0

package pricing

import (
	"bytes"
	"encoding/json"
	"math/big"
)

// maxPer is the most tokens one rate may cover, the same limit as Price.Per.
const maxPer = 1_000_000_000

// RateCard is what each kind of token costs, with the usual fallbacks. Rates are atoms (decimal strings) per Per
// tokens.
//
// Price is the wire price of an offering and has four token rates. Providers report more kinds of token than that
// (a cache write that lasts one hour, reasoning tokens) and bill a fixed fee per call. A rate card prices all of
// them from one small value, so an application can price a call with the same exact big-integer arithmetic the
// router uses, without adding a price format of its own. It does not change the wire price, the contracts or any
// store.
//
// Input, Output and Per are required. A kind left out (nil) is priced as the kind it falls back to; a rate given
// replaces the fallback, even when it is "0". Cached and CacheWrite fall back to Input, CacheWrite1h to CacheWrite
// (as resolved) and Reasoning to Output.
//
// The cost is the exact sum over every kind, rounded up once, plus the per-call fee:
//
//	atoms = ceil( (sum count_k * rate_k) / per ) + per_call
type RateCard struct {
	// Per is the tokens one rate covers, at least 1. Rates per million tokens use 1000000.
	Per int64 `json:"per"`
	// Input is for input tokens that were neither read from nor written to a cache.
	Input string `json:"input"`
	// Cached is for input tokens read from a cache. Default: Input.
	Cached *string `json:"cached,omitempty"`
	// CacheWrite is for input tokens written to the cache for the default lifetime. Default: Input.
	CacheWrite *string `json:"cache_write,omitempty"`
	// CacheWrite1h is for input tokens written to the cache for one hour. Default: CacheWrite as resolved.
	CacheWrite1h *string `json:"cache_write_1h,omitempty"`
	// Output is for visible output tokens.
	Output string `json:"output"`
	// Reasoning is for reasoning tokens the provider reports apart from the output. Default: Output.
	Reasoning *string `json:"reasoning,omitempty"`
	// PerCall is a fixed fee in atoms added once to every call, however few tokens it used. Default: none.
	PerCall *string `json:"per_call,omitempty"`
}

// TokenCounts are the measured token counts of one call. Every token is in exactly one kind, so the kinds add up to
// the call's total.
type TokenCounts struct {
	Input        int64 `json:"input"`
	Cached       int64 `json:"cached"`
	CacheWrite   int64 `json:"cache_write"`
	CacheWrite1h int64 `json:"cache_write_1h"`
	Output       int64 `json:"output"`
	Reasoning    int64 `json:"reasoning"`
}

// EffectiveRates is the rate of every kind after the fallbacks, in atoms per Per tokens.
type EffectiveRates struct {
	Per, Input, Cached, CacheWrite, CacheWrite1h, Output, Reasoning, PerCall *big.Int
}

// DecodeRateCard parses a rate card and refuses keys it does not know, with invalid_rate_card.
func DecodeRateCard(data []byte) (RateCard, error) {
	var c RateCard
	d := json.NewDecoder(bytes.NewReader(data))
	d.DisallowUnknownFields()
	if err := d.Decode(&c); err != nil {
		return RateCard{}, fail("invalid_rate_card", 400)
	}
	return c, nil
}

func invalidCard() *RouterError   { return fail("invalid_rate_card", 400) }
func invalidCounts() *RouterError { return fail("invalid_usage", 400) }

func rate(text string) (*big.Int, error) {
	if !atomsRe.MatchString(text) {
		return nil, invalidCard()
	}
	return atoms(text), nil
}

func rateOr(given *string, fallback *big.Int) (*big.Int, error) {
	if given == nil {
		return fallback, nil
	}
	return rate(*given)
}

// Effective resolves the fallbacks. It fails with invalid_rate_card when Per is outside 1..1_000_000_000 or a rate
// is not a decimal atom amount (^(0|[1-9][0-9]{0,29})$).
func (c RateCard) Effective() (EffectiveRates, error) {
	if c.Per < 1 || c.Per > maxPer {
		return EffectiveRates{}, invalidCard()
	}
	input, err := rate(c.Input)
	if err != nil {
		return EffectiveRates{}, err
	}
	output, err := rate(c.Output)
	if err != nil {
		return EffectiveRates{}, err
	}
	e := EffectiveRates{Per: big.NewInt(c.Per), Input: input, Output: output}
	if e.Cached, err = rateOr(c.Cached, input); err != nil {
		return EffectiveRates{}, err
	}
	if e.CacheWrite, err = rateOr(c.CacheWrite, input); err != nil {
		return EffectiveRates{}, err
	}
	if e.CacheWrite1h, err = rateOr(c.CacheWrite1h, e.CacheWrite); err != nil {
		return EffectiveRates{}, err
	}
	if e.Reasoning, err = rateOr(c.Reasoning, output); err != nil {
		return EffectiveRates{}, err
	}
	if e.PerCall, err = rateOr(c.PerCall, new(big.Int)); err != nil {
		return EffectiveRates{}, err
	}
	return e, nil
}

// Charge is the price of a call that used counts. It fails with invalid_usage when a count is negative.
func (c RateCard) Charge(counts TokenCounts) (*big.Int, error) {
	e, err := c.Effective()
	if err != nil {
		return nil, err
	}
	pairs := []struct {
		n    int64
		rate *big.Int
	}{{counts.Input, e.Input}, {counts.Cached, e.Cached}, {counts.CacheWrite, e.CacheWrite}, {counts.CacheWrite1h, e.CacheWrite1h}, {counts.Output, e.Output}, {counts.Reasoning, e.Reasoning}}
	sum := new(big.Int)
	for _, p := range pairs {
		if p.n < 0 {
			return nil, invalidCounts()
		}
		sum.Add(sum, new(big.Int).Mul(big.NewInt(p.n), p.rate))
	}
	total := ceilDiv(sum, e.Per)
	return total.Add(total, e.PerCall), nil
}

// Bound is the most a call can cost when its input kinds add up to at most maxInput tokens and its output and
// reasoning tokens to at most maxOutput: the dearest input rate times maxInput, the dearer of output and reasoning
// times maxOutput, and the per-call fee. Hold this before the call; Charge of any counts within those limits is
// never more. Reasoning tokens count toward maxOutput, as providers count them toward their own output limit. It
// fails with invalid_usage when a limit is negative.
func (c RateCard) Bound(maxInput, maxOutput int64) (*big.Int, error) {
	if maxInput < 0 || maxOutput < 0 {
		return nil, invalidCounts()
	}
	e, err := c.Effective()
	if err != nil {
		return nil, err
	}
	dearestInput := e.Input
	for _, r := range []*big.Int{e.Cached, e.CacheWrite, e.CacheWrite1h} {
		if r.Cmp(dearestInput) > 0 {
			dearestInput = r
		}
	}
	dearestOutput := e.Output
	if e.Reasoning.Cmp(dearestOutput) > 0 {
		dearestOutput = e.Reasoning
	}
	sum := new(big.Int).Mul(big.NewInt(maxInput), dearestInput)
	sum.Add(sum, new(big.Int).Mul(big.NewInt(maxOutput), dearestOutput))
	total := ceilDiv(sum, e.Per)
	return total.Add(total, e.PerCall), nil
}
