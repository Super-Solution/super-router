// SPDX-License-Identifier: Apache-2.0

package dataplanetest

import (
	"context"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"testing"

	"github.com/Super-Solution/super-router/backends/go/contracts"
	"github.com/Super-Solution/super-router/backends/go/internal/uuid"
	"github.com/Super-Solution/super-router/backends/go/pricing"
	"github.com/Super-Solution/super-router/backends/go/router"
)

// CustomUnits runs a task billed in a custom unit end to end: publish, a credit-rate version in that unit,
// reservation, settlement and the balance left. Rust runs the same check (store-postgres tests/units.rs).
func CustomUnits(t *testing.T, store router.DataStore) {
	t.Helper()
	raw, err := os.ReadFile(filepath.Join("..", "..", "..", "rust", "conformance", "dataplane.v1.json"))
	if err != nil {
		t.Fatal(err)
	}
	var data struct {
		Services map[string]json.RawMessage `json:"services"`
	}
	if err := json.Unmarshal(raw, &data); err != nil {
		t.Fatal(err)
	}
	service, err := contracts.ParseService(data.Services["chat"])
	if err != nil {
		t.Fatal(err)
	}
	kind := "task"
	sale := pricing.Price{
		Units: map[string]pricing.UnitRate{"pages": {Atoms: "7", Per: 3, DisplayUnit: "頁"}},
		Asset: "credit:demo", Input: "0", Cached: "0", CacheWrite: "0", Output: "0", Per: 1,
	}
	service.TaskKind = &kind
	service.Price = sale
	service.Offerings = service.Offerings[:1]
	service.Offerings[0].TaskKind = &kind
	service.Offerings[0].MaxUsage = contracts.MeterLimits{"pages": 10}
	supply := sale
	supply.Asset = "USD"
	service.Offerings[0].Price = supply
	p := contracts.Principal{Workspace: "units-" + uuid.New(), Environment: "test", Customer: "c1",
		Scopes: []string{"invoke", "service:write", "billing:write", "billing:read"}}
	ctx := context.Background()
	if _, err := store.Publish(ctx, p, service, "self-host", nil); err != nil {
		t.Fatal(err)
	}
	if _, err := store.Grant(ctx, p, "customer_sale", "credit:demo", "100", "grant", nil); err != nil {
		t.Fatal(err)
	}
	var config any
	_ = json.Unmarshal([]byte(`{"asset":"credit:demo","units":{"pages":{"units":"3000000000","credits":"7","display_unit":"頁"}}}`), &config)
	if _, err := router.PublishCreditRateVersion(ctx, store, p, "chat", 1, config, "self-host"); err != nil {
		t.Fatal(err)
	}
	// A credit-rate version may not switch a unit-billed service back to tokens.
	var tokens any
	_ = json.Unmarshal([]byte(`{"asset":"credit:demo","tokens":{"input":{"units":"1","credits":"1"},"cached":{"units":"1","credits":"1"},"cache_write":{"units":"1","credits":"1"},"output":{"units":"1","credits":"1"}}}`), &tokens)
	if _, err := router.PublishCreditRateVersion(ctx, store, p, "chat", 2, tokens, "self-host"); !isCode(err, "credit_rate_kind_mismatch") {
		t.Fatalf("tokens on a unit-billed service: %v", err)
	}
	rt, err := router.New(store, map[string]contracts.Provider{service.Offerings[0].Provider: &router.MockTaskProvider{}}, "custom-unit-test-key-at-least-thirty-two-characters")
	if err != nil {
		t.Fatal(err)
	}
	input := []byte(`{"service":"chat","customer_id":"c1","input":{"task":{"kind":"task","meter":"pages","prompt":"Print"}},"limits":{"max_usage":{"units":{"pages":4}}}}`)
	result, err := rt.Infer(ctx, p, input, "custom", nil)
	if err != nil || result.BillingStatus != "settled" {
		t.Fatalf("%v %+v", err, result)
	}
	if got := result.Usage.Meters.Units["pages"]; got != 4 {
		t.Fatalf("pages used %d", got)
	}
	// 4 pages at 7 atoms per 3 pages, rounded up, is 10 atoms.
	rows, err := store.Balance(ctx, p)
	if err != nil || len(rows) != 1 || rows[0].Balance != "90" || rows[0].Held != "0" {
		t.Fatalf("%v %+v", err, rows)
	}
	// Asking for more pages than the offering allows excludes it before anything is reserved.
	over := []byte(`{"service":"chat","customer_id":"c1","input":{"task":{"kind":"task","meter":"pages","prompt":"Print"}},"limits":{"max_usage":{"units":{"pages":11}}}}`)
	if _, err := rt.Infer(ctx, p, over, "over", nil); !isCode(err, "no_eligible_offering") {
		t.Fatalf("over the offering's limit: %v", err)
	}
}

func isCode(err error, code string) bool {
	var re *contracts.RouterError
	return errors.As(err, &re) && re.Code == code
}
