// SPDX-License-Identifier: Apache-2.0

package pricing

import "testing"

func tierCard(input, output string) RateCard {
	return RateCard{Per: 1_000_000, Input: input, Output: output}
}

func longContext(t *testing.T) RateTiers {
	t.Helper()
	tiers, err := NewRateTiers([]RateTier{
		{Above: 0, Card: tierCard("1250000", "10000000")},
		{Above: 200_000, Card: tierCard("2500000", "15000000")},
		{Above: 1_000_000, Card: tierCard("5000000", "30000000")},
	})
	if err != nil {
		t.Fatal(err)
	}
	return tiers
}

func TestTheWholeCallIsPricedByTheTierItsPromptReaches(t *testing.T) {
	tiers := longContext(t)
	for prompt, want := range map[int64]string{0: "1250000", 200_000: "1250000", 200_001: "2500000", 1_000_000: "2500000", 1_000_001: "5000000"} {
		card, err := tiers.CardFor(prompt)
		if err != nil || card.Input != want {
			t.Errorf("prompt %d: %v %v", prompt, card.Input, err)
		}
	}
	if _, err := tiers.CardFor(-1); err == nil {
		t.Error("a negative prompt was accepted")
	}
	low := TokenCounts{Input: 150_000, Cached: 50_000, Output: 10_000}
	high := low
	high.Input++
	if got, _ := tiers.Charge(low); got.String() != "350000" {
		t.Errorf("low %v", got)
	}
	// 200,001 prompt tokens at 2.5 plus 10,000 output at 15 is 650,002.5, rounded up once.
	if got, _ := tiers.Charge(high); got.String() != "650003" {
		t.Errorf("high %v", got)
	}
}

func TestAHoldCoversEveryTierThePromptCanReach(t *testing.T) {
	tiers := longContext(t)
	for _, maxInput := range []int64{0, 1, 199_999, 200_000, 200_001, 999_999, 1_000_000, 1_000_001, 3_000_000} {
		hold, err := tiers.Bound(maxInput, 50)
		if err != nil {
			t.Fatal(err)
		}
		for _, prompt := range []int64{0, 1, maxInput / 3, maxInput - 1, maxInput} {
			if prompt < 0 || prompt > maxInput {
				continue
			}
			got, err := tiers.Charge(TokenCounts{Input: prompt / 2, Cached: prompt - prompt/2, Output: 50})
			if err != nil || got.Cmp(hold) > 0 {
				t.Errorf("maxInput %d, prompt %d: charge %v, hold %v, %v", maxInput, prompt, got, hold, err)
			}
		}
	}
	if _, err := tiers.Bound(-1, 0); err == nil {
		t.Error("a negative limit was accepted")
	}
}

func TestBadTiersAreRefusedAndGoodOnesRoundTrip(t *testing.T) {
	tier := func(above int64, input string) RateTier { return RateTier{Above: above, Card: tierCard(input, "1")} }
	for _, list := range [][]RateTier{nil, {tier(1, "1")}, {tier(0, "1"), tier(0, "2")}, {tier(0, "1"), tier(9, "2"), tier(5, "3")}, {tier(0, "x")}} {
		if _, err := NewRateTiers(list); err == nil {
			t.Errorf("accepted %v", list)
		}
	}
	text := `{"tiers":[{"above":0,"card":{"per":1000,"input":"1","output":"2"}},{"above":9,"card":{"per":1000,"input":"2","output":"4"}}]}`
	tiers, err := DecodeRateTiers([]byte(text))
	if err != nil {
		t.Fatal(err)
	}
	if out, _ := tiers.MarshalJSON(); string(out) != text {
		t.Errorf("round trip %s", out)
	}
	for _, bad := range []string{`{"tiers":[]}`, `{"tiers":[],"x":1}`, `[`} {
		if _, err := DecodeRateTiers([]byte(bad)); err == nil {
			t.Errorf("accepted %s", bad)
		}
	}
	if _, err := longContext(t).Charge(TokenCounts{Cached: -1}); err == nil {
		t.Error("a negative count was accepted")
	}
}
