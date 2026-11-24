// SPDX-License-Identifier: Apache-2.0

package pricing

import (
	"encoding/json"
	"testing"
)

func card(input, output string) RateCard { return RateCard{Per: 1000, Input: input, Output: output} }

// 10/20 up to t=1000, doubled from t=1000, halved again from t=5000.
func testSchedule(t *testing.T) RateSchedule {
	t.Helper()
	s, err := NewRateSchedule([]RateStep{{0, card("10", "20")}, {1000, card("20", "40")}, {5000, card("5", "10")}})
	if err != nil {
		t.Fatal(err)
	}
	return s
}

func TestCardAtIsTheLastStepThatHasStarted(t *testing.T) {
	s := testSchedule(t)
	for at, want := range map[int64]string{0: "10", 999: "10", 1000: "20", 4999: "20", 5000: "5", 1<<63 - 1: "5"} {
		c, err := s.CardAt(at)
		if err != nil || c.Input != want {
			t.Errorf("at %d: %v %v, want %s", at, c.Input, err, want)
		}
	}
	if _, err := s.CardAt(-1); err == nil || err.(*RouterError).Code != "rate_card_not_in_effect" {
		t.Errorf("before the first step: %v", err)
	}
	for at, want := range map[int64]int64{0: 1000, 1000: 5000} {
		if next, ok := s.NextChangeAfter(at); !ok || next != want {
			t.Errorf("next change after %d: %d %v", at, next, ok)
		}
	}
	if _, ok := s.NextChangeAfter(5000); ok {
		t.Error("the last card is never replaced")
	}
}

func TestAHoldCoversAPriceChangeInTheMiddleOfACall(t *testing.T) {
	s := testSchedule(t)
	at := func(ts int64) string {
		c, _ := s.CardAt(ts)
		b, err := c.Bound(100, 100)
		if err != nil {
			t.Fatal(err)
		}
		return b.String()
	}
	for _, c := range []struct{ start, end, card int64 }{{0, 999, 0}, {1000, 4999, 1000}, {900, 1100, 1000}, {4000, 6000, 1000}, {5000, 6000, 5000}} {
		got, err := s.BoundBetween(c.start, c.end, 100, 100)
		if err != nil || got.String() != at(c.card) {
			t.Errorf("%d..%d: %v %v, want the card of %d (%s)", c.start, c.end, got, err, c.card, at(c.card))
		}
	}
}

func TestWhicheverCardPricesTheCallTheHoldIsEnough(t *testing.T) {
	s := testSchedule(t)
	counts := TokenCounts{Input: 70, Output: 30}
	for _, r := range [][2]int64{{0, 10}, {950, 1050}, {4990, 5010}, {0, 9000}, {6000, 7000}} {
		hold, err := s.BoundBetween(r[0], r[1], 100, 50)
		if err != nil {
			t.Fatal(err)
		}
		for ts := r[0]; ts <= r[1]; ts += 5 {
			c, _ := s.CardAt(ts)
			got, err := c.Charge(counts)
			if err != nil || got.Cmp(hold) > 0 {
				t.Fatalf("%v..%v priced at %d: %v %v over %v", r[0], r[1], ts, got, err, hold)
			}
		}
	}
}

func TestBadSchedulesAndRangesAreRefused(t *testing.T) {
	step := func(from int64, input string) RateStep { return RateStep{from, card(input, "1")} }
	for _, steps := range [][]RateStep{nil, {step(5, "1"), step(5, "2")}, {step(5, "1"), step(4, "2")}, {step(0, "x")}} {
		if _, err := NewRateSchedule(steps); err == nil || err.(*RouterError).Code != "invalid_rate_schedule" {
			t.Errorf("%v: %v", steps, err)
		}
	}
	s := testSchedule(t)
	for _, c := range []struct {
		start, end, in, code int64
		want                 string
	}{{10, 9, 1, 0, "invalid_usage"}, {-5, 0, 1, 0, "rate_card_not_in_effect"}, {0, 1, -1, 0, "invalid_usage"}} {
		if _, err := s.BoundBetween(c.start, c.end, c.in, 1); err == nil || err.(*RouterError).Code != c.want {
			t.Errorf("%+v: %v", c, err)
		}
	}
}

func TestAScheduleRoundTripsThroughJSONAndIsValidatedOnTheWayIn(t *testing.T) {
	text := `{"steps":[{"from_ms":0,"card":{"per":1000,"input":"1","output":"2"}},{"from_ms":9,"card":{"per":1000,"input":"2","output":"4"}}]}`
	s, err := DecodeRateSchedule([]byte(text))
	if err != nil {
		t.Fatal(err)
	}
	if out, _ := json.Marshal(s); string(out) != text {
		t.Errorf("%s", out)
	}
	for _, bad := range []string{`{"steps":[]}`, `{"steps":[],"x":1}`, `[`, `{"steps":[{"from_ms":0,"card":{"per":1,"input":"1","output":"1","cache":"1"}}]}`} {
		if _, err := DecodeRateSchedule([]byte(bad)); err == nil || err.(*RouterError).Code != "invalid_rate_schedule" {
			t.Errorf("%s: %v", bad, err)
		}
	}
}

func TestDefaultedNamesTheKindsPricedByFallback(t *testing.T) {
	kinds := func(k []TokenKind) string { b, _ := json.Marshal(k); return string(b) }
	if got := kinds(card("5", "11").Defaulted()); got != `["cached","cache_write","cache_write_1h","reasoning"]` {
		t.Errorf("%s", got)
	}
	zero, eight := "0", "8"
	c := RateCard{Per: 1, Input: "5", Output: "11", Cached: &zero, CacheWrite: &eight}
	if got := kinds(c.Defaulted()); got != `["cache_write_1h","reasoning"]` {
		t.Errorf("a rate of zero is a rate: %s", got)
	}
	counts := TokenCounts{Input: 9, Cached: 4, Output: 2}
	if got := kinds(card("5", "11").DefaultedIn(counts)); got != `["cached"]` {
		t.Errorf("%s", got)
	}
	if got := kinds(c.DefaultedIn(counts)); got != `[]` {
		t.Errorf("%s", got)
	}
}
