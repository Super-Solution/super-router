// SPDX-License-Identifier: Apache-2.0

package routing

import (
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"testing"

	"github.com/Super-Solution/super-router/backends/go/pricing"
)

type expected struct {
	Ok *struct {
		Candidates []struct {
			ID    string  `json:"id"`
			Cost  string  `json:"cost"`
			Score float64 `json:"score"`
		} `json:"candidates"`
		Excluded []Excluded `json:"excluded"`
	} `json:"ok"`
	Error string `json:"error"`
}

type fixtures struct {
	Route []struct {
		Service    Service    `json:"service"`
		Invocation Invocation `json:"invocation"`
		Expect     expected   `json:"expect"`
	} `json:"route"`
}

func TestRouteConformance(t *testing.T) {
	data, err := os.ReadFile(filepath.Join("..", "..", "..", "conformance", "routing.v1.json"))
	if err != nil {
		t.Fatal(err)
	}
	var f fixtures
	if err := json.Unmarshal(data, &f); err != nil {
		t.Fatal(err)
	}
	if len(f.Route) < 300 {
		t.Fatalf("only %d route cases", len(f.Route))
	}
	withCandidates, errorCases := 0, 0
	for i, c := range f.Route {
		got, excluded, err := Route(c.Service, c.Invocation)
		if c.Expect.Error != "" {
			errorCases++
			var re *pricing.RouterError
			if !errors.As(err, &re) || re.Code != c.Expect.Error {
				t.Errorf("route[%d]: error %v, want %s", i, err, c.Expect.Error)
			}
			continue
		}
		if err != nil {
			t.Errorf("route[%d]: %v", i, err)
			continue
		}
		want := c.Expect.Ok
		if len(got) != len(want.Candidates) {
			t.Errorf("route[%d]: %d candidates, want %d", i, len(got), len(want.Candidates))
			continue
		}
		if len(got) > 0 {
			withCandidates++
		}
		for j, cand := range got {
			w := want.Candidates[j]
			if cand.Offering.ID != w.ID || cand.Cost.String() != w.Cost || cand.Score != w.Score {
				t.Errorf("route[%d][%d]: got %s %s %v, want %s %s %v", i, j, cand.Offering.ID, cand.Cost, cand.Score, w.ID, w.Cost, w.Score)
			}
		}
		if len(excluded) != len(want.Excluded) {
			t.Errorf("route[%d]: %d excluded, want %d", i, len(excluded), len(want.Excluded))
			continue
		}
		for j := range excluded {
			if excluded[j] != want.Excluded[j] {
				t.Errorf("route[%d] excluded[%d]: %v, want %v", i, j, excluded[j], want.Excluded[j])
			}
		}
	}
	if withCandidates < 100 || errorCases < 1 {
		t.Fatalf("weak coverage: %d with candidates, %d errors", withCandidates, errorCases)
	}
}
