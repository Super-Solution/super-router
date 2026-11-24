// SPDX-License-Identifier: Apache-2.0

package pricing

import (
	"encoding/json"
	"errors"
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
