// SPDX-License-Identifier: Apache-2.0

package dataplanetest

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/Super-Solution/super-router/backends/go/contracts"
	"github.com/Super-Solution/super-router/backends/go/internal/uuid"
	"github.com/Super-Solution/super-router/backends/go/router"
)

// Downgrade checks what the conformance scenarios cannot see: the routing.downgrade stream event and, when
// decision is set, the downgrade and quality metadata the store keeps in the route decision of a request.
func Downgrade(t *testing.T, store router.DataStore, decision func(p contracts.Principal, request string) string) {
	t.Helper()
	services := rustServices(t)
	service, err := contracts.ParseService(services["chat_downgrade"])
	if err != nil {
		t.Fatal(err)
	}
	p, rt := fundedRouter(t, store, service)
	var events []contracts.StreamEvent
	res, err := rt.Infer(context.Background(), p, invocation(`"stream":true`), "downgrade", func(e contracts.StreamEvent) error {
		events = append(events, e)
		return nil
	})
	if err != nil || res.Downgrade == nil || res.Tier != "chat/basic" || res.Downgrade.FromTier != "chat/pro" {
		t.Fatalf("%v %+v", err, res)
	}
	var kinds []string
	for _, e := range events {
		kinds = append(kinds, e.Type)
		if e.Type == "routing.downgrade" {
			want := map[string]any{"from_tier": "chat/pro", "to_tier": "chat/basic", "offering": "lower"}
			if !jsonEqual(e.Data, want) {
				t.Errorf("routing.downgrade %v", e.Data)
			}
		}
	}
	if got := strings.Join(kinds, ","); got != "request.started,routing.downgrade,output.delta,request.completed" {
		t.Errorf("events %s", got)
	}
	if decision != nil {
		var d router.Decision
		if err := json.Unmarshal([]byte(decision(p, res.ID)), &d); err != nil || len(d.Candidates) != 2 {
			t.Fatalf("decision %v %+v", err, d)
		}
		primary, lower := d.Candidates[0], d.Candidates[1]
		if primary.ID != "a" || primary.Downgrade != nil || lower.ID != "lower" || lower.Downgrade == nil ||
			lower.Downgrade.ToTier != "chat/basic" || lower.QualityMetadata == nil || lower.QualityMetadata.Source != "eval-v1" {
			t.Errorf("decision %+v", d)
		}
	}
}

// StructuredOutput checks that a response_format request never reaches a provider: an offering without the
// capability is excluded, and one with it is still refused until a provider adapter writes the schema.
func StructuredOutput(t *testing.T, store router.DataStore) {
	t.Helper()
	service, err := contracts.ParseService(rustServices(t)["chat"])
	if err != nil {
		t.Fatal(err)
	}
	body := invocation(`"response_format":{"type":"json_schema","json_schema":{"name":"answer","schema":{"type":"object"}}}`)
	p, rt := fundedRouter(t, store, service)
	if _, err := rt.Infer(context.Background(), p, body, "so-1", nil); !isCode(err, "no_eligible_offering") {
		t.Fatalf("without the capability: %v", err)
	}
	caps := []string{"structured_output"}
	for i := range service.Offerings {
		service.Offerings[i].Capabilities = &caps
	}
	p, rt = fundedRouter(t, store, service)
	if _, err := rt.Infer(context.Background(), p, body, "so-2", nil); !isCode(err, "unsupported_capability") {
		t.Fatalf("with the capability: %v", err)
	}
	rows, err := store.Balance(context.Background(), p)
	if err != nil || len(rows) != 1 || rows[0].Held != "0" || rows[0].Balance != "100000" {
		t.Fatalf("nothing may be reserved: %v %+v", err, rows)
	}
}

func rustServices(t *testing.T) map[string]json.RawMessage {
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
	return data.Services
}

// fundedRouter publishes service in a fresh tenant, grants its customer 100000 credits and returns a router
// over the conformance mock providers.
func fundedRouter(t *testing.T, store router.DataStore, service contracts.Service) (contracts.Principal, *router.Router) {
	t.Helper()
	p := contracts.Principal{Workspace: "feat-" + uuid.New(), Environment: "test", Customer: "c1",
		Scopes: []string{"invoke", "service:write", "billing:write", "billing:read"}}
	ctx := context.Background()
	if _, err := store.Publish(ctx, p, service, "self-host", nil); err != nil {
		t.Fatal(err)
	}
	if _, err := store.Grant(ctx, p, "customer_sale", "credit:demo", "100000", "g1", nil); err != nil {
		t.Fatal(err)
	}
	providers := map[string]contracts.Provider{"ok": router.NewMockProvider("success"), "bad": router.NewMockProvider("reject")}
	rt, err := router.New(store, providers, "feature-test-digest-key-0123456789abcdef")
	if err != nil {
		t.Fatal(err)
	}
	return p, rt
}

// invocation is the conformance chat invocation with extra top-level fields.
func invocation(extra string) []byte {
	return []byte(`{"service":"chat","customer_id":"c1","payer_mode":"developer","input":{"messages":[{"role":"user","content":"Hello"}]},"limits":{"max_output_tokens":32,"timeout_ms":30000},` + extra + `}`)
}

func jsonEqual(a, b any) bool {
	x, _ := json.Marshal(a)
	y, _ := json.Marshal(b)
	return string(x) == string(y)
}
