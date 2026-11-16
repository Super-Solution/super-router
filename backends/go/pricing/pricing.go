// SPDX-License-Identifier: Apache-2.0

// Package pricing is the Super Router pricing core in Go.
//
// It mirrors packages/core (Charge, Bound) and packages/core/src/credit-rates.ts
// (CompileCreditPrice). Money is integer atoms held in math/big, so results match the
// TypeScript BigInt reference exactly. conformance/pricing.v1.json is the shared test data.
package pricing

import (
	"encoding/json"
	"fmt"
	"math/big"
	"regexp"
	"sort"
)

// CreditAtoms is the fixed accounting scale of one credit.
const CreditAtoms = 1_000_000_000

var (
	tokenKeys = []string{"input", "cached", "cache_write", "output"}
	mediaKeys = []string{"images", "video_ms", "audio_ms", "tasks"}

	integerRe = regexp.MustCompile(`^[1-9][0-9]{0,29}$`)
	atomsRe   = regexp.MustCompile(`^(0|[1-9][0-9]{0,29})$`)
	idRe      = regexp.MustCompile(`^[a-zA-Z0-9_.:/-]+$`)
)

// RouterError carries a stable error code, like the TypeScript RouterError.
type RouterError struct {
	Code   string
	Status int
	// Retryable reports that the same call can succeed later without changes (a window that resets).
	Retryable bool
	// Details are safe, customer-visible facts about the refusal, such as which window and when it resets.
	Details map[string]any
}

func (e *RouterError) Error() string { return e.Code }

func fail(code string, status int) *RouterError { return &RouterError{Code: code, Status: status} }

// MediaRate prices one media meter: Atoms per Per units.
type MediaRate struct {
	Atoms string `json:"atoms"`
	Per   int64  `json:"per"`
}

// Price is the wire price. Token rates are atoms per Per tokens.
type Price struct {
	Asset      string               `json:"asset"`
	Input      string               `json:"input"`
	Cached     string               `json:"cached"`
	CacheWrite string               `json:"cache_write"`
	Output     string               `json:"output"`
	Per        int64                `json:"per"`
	Media      map[string]MediaRate `json:"media,omitempty"`
}

// Usage holds meter readings; media meters absent from the map count as zero.
type Usage struct {
	Input, Cached, CacheWrite, Output int64
	Media                             map[string]int64
}

func atoms(s string) *big.Int {
	n, ok := new(big.Int).SetString(s, 10)
	if !ok {
		panic(fmt.Sprintf("pricing: %q is not a decimal integer", s))
	}
	return n
}

func ceilDiv(numerator, denominator *big.Int) *big.Int {
	n := new(big.Int).Add(numerator, denominator)
	n.Sub(n, big.NewInt(1))
	return n.Quo(n, denominator)
}

// Charge is the customer charge in atoms for one usage record.
func Charge(u Usage, p Price) (*big.Int, error) {
	pairs := []struct {
		units int64
		rate  string
	}{{u.Input, p.Input}, {u.Cached, p.Cached}, {u.CacheWrite, p.CacheWrite}, {u.Output, p.Output}}
	numerator := new(big.Int)
	for _, pair := range pairs {
		numerator.Add(numerator, new(big.Int).Mul(big.NewInt(pair.units), atoms(pair.rate)))
	}
	total := ceilDiv(numerator, big.NewInt(p.Per))
	for _, key := range mediaKeys {
		units := u.Media[key]
		rate, priced := p.Media[key]
		if units != 0 && !priced {
			return nil, fail("unpriced_meter", 422)
		}
		if priced {
			part := new(big.Int).Mul(big.NewInt(units), atoms(rate.Atoms))
			total.Add(total, ceilDiv(part, big.NewInt(rate.Per)))
		}
	}
	return total, nil
}

// Bound is the upper bound reserved before a call: the dearest input rate times input, plus output.
func Bound(input, output int64, p Price) *big.Int {
	rate := atoms(p.Input)
	for _, other := range []string{p.Cached, p.CacheWrite} {
		if o := atoms(other); o.Cmp(rate) > 0 {
			rate = o
		}
	}
	sum := new(big.Int).Mul(big.NewInt(input), rate)
	sum.Add(sum, new(big.Int).Mul(big.NewInt(output), atoms(p.Output)))
	return ceilDiv(sum, big.NewInt(p.Per))
}

var invalid = func() *RouterError { return fail("invalid_credit_rates", 400) }

type ratio struct{ units, credits *big.Int }

func parseRatio(v any) (ratio, error) {
	m, ok := v.(map[string]any)
	if !ok {
		return ratio{}, invalid()
	}
	for k := range m {
		if k != "units" && k != "credits" {
			return ratio{}, invalid()
		}
	}
	units, hasUnits := m["units"]
	if !hasUnits {
		return ratio{}, invalid()
	}
	credits, hasCredits := m["credits"]
	if !hasCredits {
		credits = "1"
	}
	var out [2]*big.Int
	for i, item := range []any{units, credits} {
		s, ok := item.(string)
		if !ok || !integerRe.MatchString(s) {
			return ratio{}, invalid()
		}
		out[i] = atoms(s)
	}
	return ratio{out[0], out[1]}, nil
}

func reduce(r ratio) (numerator, denominator *big.Int) {
	numerator = new(big.Int).Mul(big.NewInt(CreditAtoms), r.credits)
	denominator = new(big.Int).Set(r.units)
	g := new(big.Int).GCD(nil, nil, numerator, denominator)
	return numerator.Quo(numerator, g), denominator.Quo(denominator, g)
}

func keysWithin(m map[string]any, allowed []string) bool {
	set := map[string]bool{}
	for _, k := range allowed {
		set[k] = true
	}
	for k := range m {
		if !set[k] {
			return false
		}
	}
	return true
}

// CompileCreditPrice compiles exact rational units-per-credit into the wire Price.
// raw is a decoded JSON value (use json.Unmarshal into any).
func CompileCreditPrice(raw any) (Price, error) {
	cfg, ok := raw.(map[string]any)
	if !ok || !keysWithin(cfg, []string{"asset", "tokens", "media"}) {
		return Price{}, invalid()
	}
	asset, ok := cfg["asset"].(string)
	if !ok || len(asset) < 1 || len(asset) > 120 || !idRe.MatchString(asset) {
		return Price{}, invalid()
	}
	var tokens map[string]ratio
	if v, present := cfg["tokens"]; present {
		m, ok := v.(map[string]any)
		if !ok || len(m) != len(tokenKeys) || !keysWithin(m, tokenKeys) {
			return Price{}, invalid()
		}
		tokens = map[string]ratio{}
		for _, k := range tokenKeys {
			r, err := parseRatio(m[k])
			if err != nil {
				return Price{}, err
			}
			tokens[k] = r
		}
	}
	media := map[string]ratio{}
	if v, present := cfg["media"]; present {
		m, ok := v.(map[string]any)
		if !ok || !keysWithin(m, mediaKeys) {
			return Price{}, invalid()
		}
		for k, item := range m {
			r, err := parseRatio(item)
			if err != nil {
				return Price{}, err
			}
			media[k] = r
		}
	}
	if tokens == nil && len(media) == 0 {
		return Price{}, invalid()
	}

	numerators := make([]*big.Int, len(tokenKeys))
	denominators := make([]*big.Int, len(tokenKeys))
	per := big.NewInt(1)
	limit := big.NewInt(1_000_000_000)
	for i, k := range tokenKeys {
		if tokens != nil {
			numerators[i], denominators[i] = reduce(tokens[k])
		} else {
			numerators[i], denominators[i] = big.NewInt(0), big.NewInt(1)
		}
		g := new(big.Int).GCD(nil, nil, per, denominators[i])
		per = new(big.Int).Mul(new(big.Int).Quo(per, g), denominators[i])
		if per.Cmp(limit) > 0 {
			return Price{}, fail("unsupported_rate_precision", 422)
		}
	}
	out := Price{Asset: asset, Per: per.Int64()}
	values := make([]string, len(tokenKeys))
	for i := range tokenKeys {
		values[i] = new(big.Int).Mul(numerators[i], new(big.Int).Quo(per, denominators[i])).String()
	}
	out.Input, out.Cached, out.CacheWrite, out.Output = values[0], values[1], values[2], values[3]
	mediaNames := make([]string, 0, len(media))
	for k := range media {
		mediaNames = append(mediaNames, k)
	}
	sort.Strings(mediaNames)
	for _, k := range mediaNames {
		numerator, denominator := reduce(media[k])
		if denominator.Cmp(limit) > 0 {
			return Price{}, fail("unsupported_rate_precision", 422)
		}
		if out.Media == nil {
			out.Media = map[string]MediaRate{}
		}
		out.Media[k] = MediaRate{Atoms: numerator.String(), Per: denominator.Int64()}
	}
	// The wire Price caps atoms at 30 digits.
	check := append([]string{}, values...)
	for _, r := range out.Media {
		check = append(check, r.Atoms)
	}
	for _, a := range check {
		if !atomsRe.MatchString(a) {
			return Price{}, fail("unsupported_rate_precision", 422)
		}
	}
	return out, nil
}

// DecodePrice reads a wire price from JSON.
func DecodePrice(data []byte) (Price, error) {
	var p Price
	err := json.Unmarshal(data, &p)
	return p, err
}
