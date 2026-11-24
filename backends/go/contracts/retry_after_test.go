// SPDX-License-Identifier: Apache-2.0

package contracts

import (
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
)

func TestRetryAfterConformance(t *testing.T) {
	raw, err := os.ReadFile(filepath.Join("..", "..", "rust", "conformance", "retry_after.v1.json"))
	if err != nil {
		t.Fatal(err)
	}
	var data struct {
		Cases []struct {
			Header   string `json:"header"`
			ExpectMs *int64 `json:"expect_ms"`
		} `json:"cases"`
	}
	if err := json.Unmarshal(raw, &data); err != nil {
		t.Fatal(err)
	}
	if len(data.Cases) < 30 {
		t.Fatalf("only %d cases", len(data.Cases))
	}
	for _, c := range data.Cases {
		ms, ok := ParseRetryAfter(c.Header)
		switch {
		case c.ExpectMs == nil && ok:
			t.Errorf("%q: have %d, want nothing", c.Header, ms)
		case c.ExpectMs != nil && (!ok || ms != *c.ExpectMs):
			t.Errorf("%q: have %d (%v), want %d", c.Header, ms, ok, *c.ExpectMs)
		}
	}
}

func TestTheRetryAfterCapIsOneDay(t *testing.T) {
	if RetryAfterMaxMs != 24*60*60*1000 {
		t.Fatal(RetryAfterMaxMs)
	}
}
