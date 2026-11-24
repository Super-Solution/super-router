// SPDX-License-Identifier: Apache-2.0

package pricing

import (
	"encoding/json"
	"errors"
	"math/big"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"testing"
)

type expect struct {
	Ok    any    `json:"ok"`
	Error string `json:"error"`
}

// usage is a usage record as the file writes it: meters at the top level, custom units under "units".
type usage struct {
	meters map[string]int64
	units  map[string]int64
}

func (u *usage) UnmarshalJSON(data []byte) error {
	var raw map[string]json.RawMessage
	if err := json.Unmarshal(data, &raw); err != nil {
		return err
	}
	u.meters = map[string]int64{}
	for k, v := range raw {
		if k == "units" {
			if err := json.Unmarshal(v, &u.units); err != nil {
				return err
			}
			continue
		}
		var n int64
		if err := json.Unmarshal(v, &n); err != nil {
			return err
		}
		u.meters[k] = n
	}
	return nil
}

type fixtures struct {
	Charge []struct {
		Price  Price  `json:"price"`
		Usage  usage  `json:"usage"`
		Expect expect `json:"expect"`
	} `json:"charge"`
	Bound []struct {
		Price  Price  `json:"price"`
		Input  int64  `json:"input"`
		Output int64  `json:"output"`
		Expect expect `json:"expect"`
	} `json:"bound"`
	Compile []struct {
		Config json.RawMessage `json:"config"`
		Expect expect          `json:"expect"`
	} `json:"compile_credit_price"`
	RateCardCharge []struct {
		Card   json.RawMessage `json:"card"`
		Usage  json.RawMessage `json:"usage"`
		Expect expect          `json:"expect"`
	} `json:"rate_card_charge"`
	RateCardBound []struct {
		Card   json.RawMessage `json:"card"`
		Input  int64           `json:"input"`
		Output int64           `json:"output"`
		Expect expect          `json:"expect"`
	} `json:"rate_card_bound"`
	RateScheduleCharge []struct {
		Schedule json.RawMessage `json:"schedule"`
		AtMs     int64           `json:"at_ms"`
		Usage    json.RawMessage `json:"usage"`
		Expect   expect          `json:"expect"`
	} `json:"rate_schedule_charge"`
	RateScheduleBound []struct {
		Schedule json.RawMessage `json:"schedule"`
		StartMs  int64           `json:"start_ms"`
		EndMs    int64           `json:"end_ms"`
		Input    int64           `json:"input"`
		Output   int64           `json:"output"`
		Expect   expect          `json:"expect"`
	} `json:"rate_schedule_bound"`
	RateCardDefaulted []struct {
		Card   json.RawMessage `json:"card"`
		Usage  json.RawMessage `json:"usage"`
		Expect struct {
			All  []TokenKind `json:"all"`
			Used []TokenKind `json:"used"`
		} `json:"expect"`
	} `json:"rate_card_defaulted"`
}

func load(t *testing.T) fixtures {
	t.Helper()
	// The Rust copy holds every shared case plus the custom-unit and downgrade cases TypeScript does not have.
	data, err := os.ReadFile(filepath.Join("..", "..", "rust", "conformance", "pricing.v1.json"))
	if err != nil {
		t.Fatal(err)
	}
	var f fixtures
	if err := json.Unmarshal(data, &f); err != nil {
		t.Fatal(err)
	}
	return f
}

func code(err error) string {
	var re *RouterError
	if errors.As(err, &re) {
		return re.Code
	}
	return "unexpected: " + err.Error()
}

func TestCharge(t *testing.T) {
	f := load(t)
	if len(f.Charge) < 100 {
		t.Fatalf("only %d charge cases", len(f.Charge))
	}
	for i, c := range f.Charge {
		m := c.Usage.meters
		u := Usage{Input: m["input"], Cached: m["cached"], CacheWrite: m["cache_write"], Output: m["output"], Media: map[string]int64{}, Units: c.Usage.units}
		for _, k := range mediaKeys {
			u.Media[k] = m[k]
		}
		got, err := Charge(u, c.Price)
		switch {
		case c.Expect.Error != "":
			if code(err) != c.Expect.Error {
				t.Errorf("charge[%d]: error %v, want %s", i, err, c.Expect.Error)
			}
		case err != nil:
			t.Errorf("charge[%d]: %v", i, err)
		case got.String() != c.Expect.Ok:
			t.Errorf("charge[%d]: %s, want %v", i, got, c.Expect.Ok)
		}
	}
}

func TestBound(t *testing.T) {
	for i, c := range load(t).Bound {
		if got := Bound(c.Input, c.Output, c.Price).String(); got != c.Expect.Ok {
			t.Errorf("bound[%d]: %s, want %v", i, got, c.Expect.Ok)
		}
	}
}

func TestCompileCreditPrice(t *testing.T) {
	f := load(t)
	errorsSeen := 0
	for i, c := range f.Compile {
		var config any
		if err := json.Unmarshal(c.Config, &config); err != nil {
			t.Fatal(err)
		}
		got, err := CompileCreditPrice(config)
		if c.Expect.Error != "" {
			errorsSeen++
			if code(err) != c.Expect.Error {
				t.Errorf("compile[%d]: error %v, want %s", i, err, c.Expect.Error)
			}
			continue
		}
		if err != nil {
			t.Errorf("compile[%d]: %v", i, err)
			continue
		}
		gotJSON, _ := json.Marshal(got)
		var gotAny any
		_ = json.Unmarshal(gotJSON, &gotAny)
		if !reflect.DeepEqual(gotAny, normalise(c.Expect.Ok)) {
			t.Errorf("compile[%d]: %s, want %v", i, gotJSON, c.Expect.Ok)
		}
	}
	if errorsSeen < 10 {
		t.Fatalf("only %d error cases", errorsSeen)
	}
}

// normalise round-trips through JSON so numbers compare as float64 on both sides.
func normalise(v any) any {
	data, _ := json.Marshal(v)
	var out any
	_ = json.Unmarshal(data, &out)
	return out
}

func TestValidUnitName(t *testing.T) {
	for _, good := range []string{"tool_calls", "pages", strings.Repeat("a", 64)} {
		if !ValidUnitName(good) {
			t.Errorf("%q refused", good)
		}
	}
	for _, bad := range []string{"", "Units", "units", "input", "images", "__proto__", "1a", "a-b", "constructor", strings.Repeat("a", 65)} {
		if ValidUnitName(bad) {
			t.Errorf("%q accepted", bad)
		}
	}
}

// checkExpect compares a result with a rate card case: an error code, or the amount as a decimal string.
func checkExpect(t *testing.T, name string, i int, got *big.Int, err error, want expect) {
	t.Helper()
	switch {
	case want.Error != "":
		if code(err) != want.Error {
			t.Errorf("%s[%d]: error %v, want %s", name, i, err, want.Error)
		}
	case err != nil:
		t.Errorf("%s[%d]: %v", name, i, err)
	case got.String() != want.Ok:
		t.Errorf("%s[%d]: %s, want %v", name, i, got, want.Ok)
	}
}

func TestRateCardCharge(t *testing.T) {
	f := load(t)
	errorsSeen := 0
	for i, c := range f.RateCardCharge {
		card, err := DecodeRateCard(c.Card)
		if err != nil {
			t.Fatalf("rate_card_charge[%d]: %v", i, err)
		}
		var counts TokenCounts
		if err := json.Unmarshal(c.Usage, &counts); err != nil {
			t.Fatal(err)
		}
		got, err := card.Charge(counts)
		if c.Expect.Error != "" {
			errorsSeen++
		}
		checkExpect(t, "rate_card_charge", i, got, err, c.Expect)
	}
	if len(f.RateCardCharge) < 30 || errorsSeen < 10 {
		t.Fatalf("%d cases, %d errors", len(f.RateCardCharge), errorsSeen)
	}
}

func TestRateCardBound(t *testing.T) {
	f := load(t)
	errorsSeen := 0
	for i, c := range f.RateCardBound {
		card, err := DecodeRateCard(c.Card)
		if err != nil {
			t.Fatalf("rate_card_bound[%d]: %v", i, err)
		}
		got, err := card.Bound(c.Input, c.Output)
		if c.Expect.Error != "" {
			errorsSeen++
		}
		checkExpect(t, "rate_card_bound", i, got, err, c.Expect)
	}
	if len(f.RateCardBound) < 10 || errorsSeen < 4 {
		t.Fatalf("%d cases, %d errors", len(f.RateCardBound), errorsSeen)
	}
}

func TestRateScheduleCharge(t *testing.T) {
	f := load(t)
	errorsSeen := 0
	for i, c := range f.RateScheduleCharge {
		var counts TokenCounts
		if err := json.Unmarshal(c.Usage, &counts); err != nil {
			t.Fatal(err)
		}
		var got *big.Int
		schedule, err := DecodeRateSchedule(c.Schedule)
		if err == nil {
			var card RateCard
			if card, err = schedule.CardAt(c.AtMs); err == nil {
				got, err = card.Charge(counts)
			}
		}
		if c.Expect.Error != "" {
			errorsSeen++
		}
		checkExpect(t, "rate_schedule_charge", i, got, err, c.Expect)
	}
	if len(f.RateScheduleCharge) < 12 || errorsSeen < 6 {
		t.Fatalf("%d cases, %d errors", len(f.RateScheduleCharge), errorsSeen)
	}
}

func TestRateScheduleBound(t *testing.T) {
	f := load(t)
	errorsSeen := 0
	for i, c := range f.RateScheduleBound {
		schedule, err := DecodeRateSchedule(c.Schedule)
		if err != nil {
			t.Fatalf("rate_schedule_bound[%d]: %v", i, err)
		}
		got, err := schedule.BoundBetween(c.StartMs, c.EndMs, c.Input, c.Output)
		if c.Expect.Error != "" {
			errorsSeen++
		}
		checkExpect(t, "rate_schedule_bound", i, got, err, c.Expect)
	}
	if len(f.RateScheduleBound) < 8 || errorsSeen < 2 {
		t.Fatalf("%d cases, %d errors", len(f.RateScheduleBound), errorsSeen)
	}
}

func TestRateCardDefaulted(t *testing.T) {
	f := load(t)
	if len(f.RateCardDefaulted) < 4 {
		t.Fatalf("%d cases", len(f.RateCardDefaulted))
	}
	for i, c := range f.RateCardDefaulted {
		card, err := DecodeRateCard(c.Card)
		if err != nil {
			t.Fatal(err)
		}
		var counts TokenCounts
		if err := json.Unmarshal(c.Usage, &counts); err != nil {
			t.Fatal(err)
		}
		if got := card.Defaulted(); !reflect.DeepEqual(got, c.Expect.All) {
			t.Errorf("rate_card_defaulted[%d] all: %v, want %v", i, got, c.Expect.All)
		}
		if got := card.DefaultedIn(counts); !reflect.DeepEqual(got, c.Expect.Used) {
			t.Errorf("rate_card_defaulted[%d] used: %v, want %v", i, got, c.Expect.Used)
		}
	}
}
