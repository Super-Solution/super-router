// SPDX-License-Identifier: Apache-2.0

package routing

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"testing"

	"github.com/Super-Solution/super-router/backends/go/contracts"
)

func TestHealthConformance(t *testing.T) {
	raw, err := os.ReadFile(filepath.Join("..", "..", "rust", "conformance", "health.v1.json"))
	if err != nil {
		t.Fatal(err)
	}
	var data struct {
		Cases []struct {
			Name   string           `json:"name"`
			Policy map[string]int64 `json:"policy"`
			Steps  []struct {
				Op     string `json:"op"`
				ID     string `json:"id"`
				At     int64  `json:"at"`
				Class  string `json:"class"`
				Expect struct {
					Result  string `json:"result"`
					UntilMs int64  `json:"until_ms"`
					Class   string `json:"class"`
				} `json:"expect"`
			} `json:"steps"`
			FinalSize int `json:"final_size"`
		} `json:"cases"`
	}
	if err := json.Unmarshal(raw, &data); err != nil {
		t.Fatal(err)
	}
	if len(data.Cases) < 12 {
		t.Fatalf("only %d cases", len(data.Cases))
	}
	for _, c := range data.Cases {
		p := DefaultHealthPolicy()
		set := func(k string, to *int64) {
			if v, ok := c.Policy[k]; ok {
				*to = v
			}
		}
		set("rate_limited_ms", &p.RateLimitedMs)
		set("quota_exhausted_ms", &p.QuotaExhaustedMs)
		set("credentials_rejected_ms", &p.CredentialsRejectedMs)
		set("model_unavailable_ms", &p.ModelUnavailableMs)
		set("region_blocked_ms", &p.RegionBlockedMs)
		set("max_ms", &p.MaxMs)
		set("probe_lease_ms", &p.ProbeLeaseMs)
		book := NewHealthBook(p)
		for i, s := range c.Steps {
			switch s.Op {
			case "admit":
				got := book.Admit(s.ID, s.At)
				var have string
				switch got.Kind {
				case AdmitOpen:
					have = "open"
				case AdmitProbe:
					have = "probe"
				default:
					have = "cooling"
				}
				bad := have != s.Expect.Result
				if have == "cooling" && (got.UntilMs != s.Expect.UntilMs || got.Class.Name() != s.Expect.Class) {
					bad = true
				}
				if bad {
					t.Errorf("%s step %d: have %s %s until %d, want %+v", c.Name, i, have, got.Class.Name(), got.UntilMs, s.Expect)
				}
			case "fail":
				class, ok := contracts.ParseFailureClass(s.Class)
				if !ok {
					t.Fatalf("%s: unknown class %q", c.Name, s.Class)
				}
				book.Failed(s.ID, class, s.At)
			case "succeed":
				book.Succeeded(s.ID)
			default:
				t.Fatal(fmt.Sprintf("unknown op %q", s.Op))
			}
		}
		if book.Len() != c.FinalSize {
			t.Errorf("%s: %d records, want %d", c.Name, book.Len(), c.FinalSize)
		}
	}
}
