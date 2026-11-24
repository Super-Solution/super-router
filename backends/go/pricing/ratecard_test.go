// SPDX-License-Identifier: Apache-2.0

package pricing

import (
	"math/rand"
	"testing"
)

func ptr(s string) *string { return &s }

// A card with no extra kind and no fee is exactly Charge and Bound of the wire price with the same four rates.
func TestRateCardWithoutExtrasIsTheWirePrice(t *testing.T) {
	card := RateCard{Per: 7, Input: "5", Cached: ptr("3"), CacheWrite: ptr("9"), Output: "11"}
	price := Price{Asset: "credits", Input: "5", Cached: "3", CacheWrite: "9", Output: "11", Per: 7}
	r := rand.New(rand.NewSource(20261124))
	for i := 0; i < 500; i++ {
		counts := TokenCounts{Input: r.Int63n(5000), Cached: r.Int63n(5000), CacheWrite: r.Int63n(5000), Output: r.Int63n(5000)}
		got, err := card.Charge(counts)
		if err != nil {
			t.Fatal(err)
		}
		want, err := Charge(Usage{Input: counts.Input, Cached: counts.Cached, CacheWrite: counts.CacheWrite, Output: counts.Output}, price)
		if err != nil {
			t.Fatal(err)
		}
		if got.Cmp(want) != 0 {
			t.Fatalf("charge %v: %s, want %s", counts, got, want)
		}
		in, out := r.Int63n(100000), r.Int63n(100000)
		gotBound, err := card.Bound(in, out)
		if err != nil {
			t.Fatal(err)
		}
		if gotBound.Cmp(Bound(in, out, price)) != 0 {
			t.Fatalf("bound %d/%d: %s, want %s", in, out, gotBound, Bound(in, out, price))
		}
	}
}

// Whatever the counts, a call within its limits never costs more than was held for it.
func TestRateCardChargeWithinTheLimitsNeverExceedsTheBound(t *testing.T) {
	card := RateCard{
		Per: 1000, Input: "30", Cached: ptr("2"), CacheWrite: ptr("40"), CacheWrite1h: ptr("90"),
		Output: "100", Reasoning: ptr("120"), PerCall: ptr("13"),
	}
	r := rand.New(rand.NewSource(7))
	for i := 0; i < 1000; i++ {
		maxIn, maxOut := r.Int63n(20000), r.Int63n(20000)
		cuts := []int64{r.Int63n(maxIn + 1), r.Int63n(maxIn + 1), r.Int63n(maxIn + 1)}
		if cuts[0] > cuts[1] {
			cuts[0], cuts[1] = cuts[1], cuts[0]
		}
		if cuts[1] > cuts[2] {
			cuts[1], cuts[2] = cuts[2], cuts[1]
		}
		if cuts[0] > cuts[1] {
			cuts[0], cuts[1] = cuts[1], cuts[0]
		}
		reasoning := r.Int63n(maxOut + 1)
		counts := TokenCounts{Input: cuts[0], Cached: cuts[1] - cuts[0], CacheWrite: cuts[2] - cuts[1], CacheWrite1h: maxIn - cuts[2], Output: maxOut - reasoning, Reasoning: reasoning}
		charged, err := card.Charge(counts)
		if err != nil {
			t.Fatal(err)
		}
		held, err := card.Bound(maxIn, maxOut)
		if err != nil {
			t.Fatal(err)
		}
		if charged.Cmp(held) > 0 {
			t.Fatalf("%v within %d/%d charged %s, held %s", counts, maxIn, maxOut, charged, held)
		}
	}
}

func TestRateCardEffectiveRatesResolveTheFallbacksInOrder(t *testing.T) {
	e, err := RateCard{Per: 1, Input: "5", Output: "11"}.Effective()
	if err != nil {
		t.Fatal(err)
	}
	for name, got := range map[string]string{"cached": e.Cached.String(), "cache_write": e.CacheWrite.String(), "cache_write_1h": e.CacheWrite1h.String()} {
		if got != "5" {
			t.Errorf("%s = %s, want 5", name, got)
		}
	}
	if e.Reasoning.String() != "11" || e.PerCall.String() != "0" {
		t.Errorf("reasoning %s, per_call %s", e.Reasoning, e.PerCall)
	}
	e, _ = RateCard{Per: 1, Input: "5", Output: "11", CacheWrite: ptr("8")}.Effective()
	if e.CacheWrite1h.String() != "8" {
		t.Errorf("cache_write_1h = %s, want 8", e.CacheWrite1h)
	}
	// a rate of zero is a rate, not a missing one
	e, _ = RateCard{Per: 1, Input: "5", Output: "11", Cached: ptr("0")}.Effective()
	if e.Cached.String() != "0" {
		t.Errorf("cached = %s, want 0", e.Cached)
	}
}

func TestDecodeRateCardRefusesUnknownKeys(t *testing.T) {
	if _, err := DecodeRateCard([]byte(`{"per":1000000,"input":"1","output":"2","reasoning":"3","per_call":"4"}`)); err != nil {
		t.Fatal(err)
	}
	for _, bad := range []string{`{"per":1,"input":"1","output":"1","cache":"1"}`, `[]`, `{"per":"1","input":"1","output":"1"}`} {
		if _, err := DecodeRateCard([]byte(bad)); code(err) != "invalid_rate_card" {
			t.Errorf("%s: %v", bad, err)
		}
	}
}
