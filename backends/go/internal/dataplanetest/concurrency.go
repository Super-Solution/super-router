// SPDX-License-Identifier: Apache-2.0

package dataplanetest

import (
	"context"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"strconv"
	"sync"
	"testing"
	"time"

	"github.com/Super-Solution/super-router/backends/go/contracts"
	"github.com/Super-Solution/super-router/backends/go/internal/uuid"
	"github.com/Super-Solution/super-router/backends/go/router"
)

// Concurrency fires more simultaneous requests than the balance can hold and checks the invariants that must
// survive any interleaving: no overspend, no lost update, nothing left held, and every refusal is a 402.
func Concurrency(t *testing.T, h *Harness) {
	t.Helper()
	// The Rust copy holds every shared scenario plus the ones for features the TypeScript reference lacks.
	raw, err := os.ReadFile(filepath.Join("..", "..", "..", "rust", "conformance", "dataplane.v1.json"))
	if err != nil {
		t.Fatal(err)
	}
	var data struct {
		Services   map[string]json.RawMessage `json:"services"`
		Invocation json.RawMessage            `json:"invocation"`
	}
	if err := json.Unmarshal(raw, &data); err != nil {
		t.Fatal(err)
	}
	service, err := contracts.ParseService(data.Services["chat"])
	if err != nil {
		t.Fatal(err)
	}
	p := contracts.Principal{Workspace: "ws-" + uuid.New(), Environment: "test", Customer: "c1",
		Scopes: []string{"invoke", "service:write", "billing:write", "billing:read"}}
	ctx := context.Background()
	if _, err := h.Store.Publish(ctx, p, service, "", nil); err != nil {
		t.Fatal(err)
	}
	// One request reserves 1024*2 + 32*3 = 2144 atoms and costs 35 once settled. Room for ten holds.
	const hold, cost, workers = 2144, 35, 24
	grant := strconv.Itoa(hold * 10)
	if _, err := h.Store.Grant(ctx, p, "customer_sale", "credit:demo", grant, "g1", nil); err != nil {
		t.Fatal(err)
	}
	mock := router.NewMockProvider("success")
	mock.Delay = 400 * time.Millisecond
	rt, err := router.New(h.Store, map[string]contracts.Provider{"ok": mock}, DigestKey, router.WithClock(h.Clock))
	if err != nil {
		t.Fatal(err)
	}
	var wg sync.WaitGroup
	results := make([]error, workers)
	for i := 0; i < workers; i++ {
		wg.Add(1)
		go func(i int) {
			defer wg.Done()
			_, results[i] = rt.Infer(ctx, p, data.Invocation, "key-"+strconv.Itoa(i), nil)
		}(i)
	}
	wg.Wait()
	settled, refused := 0, 0
	for _, err := range results {
		var re *contracts.RouterError
		switch {
		case err == nil:
			settled++
		case errors.As(err, &re) && re.Code == "insufficient_balance" && re.Status == 402:
			refused++
		default:
			t.Errorf("unexpected error: %v", err)
		}
	}
	if settled < 10 || refused < 1 || settled+refused != workers {
		t.Fatalf("%d settled and %d refused of %d", settled, refused, workers)
	}
	if got := int(mock.Calls()); got != settled {
		t.Errorf("provider called %d times for %d admitted requests", got, settled)
	}
	rows, err := h.Store.Balance(ctx, p)
	if err != nil || len(rows) != 1 {
		t.Fatalf("balance %v %v", rows, err)
	}
	if want := strconv.Itoa(hold*10 - cost*settled); rows[0].Balance != want || rows[0].Held != "0" {
		t.Errorf("balance %s held %s, want %s held 0", rows[0].Balance, rows[0].Held, want)
	}
}
