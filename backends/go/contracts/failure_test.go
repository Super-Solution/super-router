// SPDX-License-Identifier: Apache-2.0

package contracts

import (
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
)

func TestFailureConformance(t *testing.T) {
	raw, err := os.ReadFile(filepath.Join("..", "..", "rust", "conformance", "failure.v1.json"))
	if err != nil {
		t.Fatal(err)
	}
	var data struct {
		Cases []struct {
			Name    string `json:"name"`
			Status  int    `json:"status"`
			Message string `json:"message"`
			Expect  struct {
				Class          string `json:"class"`
				Definitive     bool   `json:"definitive"`
				Retryable      bool   `json:"retryable"`
				Code           string `json:"code"`
				OfferingScoped bool   `json:"offering_scoped"`
			} `json:"expect"`
		} `json:"cases"`
	}
	if err := json.Unmarshal(raw, &data); err != nil {
		t.Fatal(err)
	}
	if len(data.Cases) < 40 {
		t.Fatalf("only %d cases", len(data.Cases))
	}
	for _, c := range data.Cases {
		class := ClassifyFailure(c.Status, c.Message)
		e := class.ProviderError(c.Status)
		if class.Name() != c.Expect.Class || e.Definitive != c.Expect.Definitive || e.Retryable != c.Expect.Retryable ||
			e.Code != c.Expect.Code || class.IsOfferingScoped() != c.Expect.OfferingScoped {
			t.Errorf("%s: have %s %+v scoped=%v, want %+v", c.Name, class.Name(), *e, class.IsOfferingScoped(), c.Expect)
		}
	}
}
