// SPDX-License-Identifier: Apache-2.0

// Package dataplanetest runs conformance/dataplane.v1.json against any router.DataStore. The same file drives
// the TypeScript and Python implementations, so every store behaves identically.
package dataplanetest

import (
	"context"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"reflect"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/Super-Solution/super-router/backends/go/contracts"
	"github.com/Super-Solution/super-router/backends/go/internal/uuid"
	"github.com/Super-Solution/super-router/backends/go/router"
)

// DigestKey keys the idempotency digest in every scenario.
const DigestKey = "conformance-digest-key-0123456789abcdef"

type obj = map[string]any

// BucketRow is one funding bucket as the scenarios compare it.
type BucketRow struct {
	Source    string `json:"source"`
	Remaining string `json:"remaining"`
	Held      string `json:"held"`
}

// Harness is a store under test plus the means to control and observe it.
type Harness struct {
	Store router.DataStore
	// Clock is "now" for the store: virtual for in-memory stores, the real one for SQL stores.
	Clock router.Clock
	// Advance moves time forward by ms (virtually or by sleeping).
	Advance func(ms int64)
	// Buckets lists a tenant's funding buckets ordered by source.
	Buckets func(p contracts.Principal) []BucketRow
}

// Run executes every scenario of the file as a subtest. make builds a fresh harness for one scenario.
func Run(t *testing.T, name string, make func(t *testing.T) *Harness) {
	t.Helper()
	// The Rust copy holds every shared scenario plus the ones for features the TypeScript reference lacks.
	raw, err := os.ReadFile(filepath.Join("..", "..", "..", "rust", "conformance", "dataplane.v1.json"))
	if err != nil {
		t.Fatal(err)
	}
	var data obj
	if err := json.Unmarshal(raw, &data); err != nil {
		t.Fatal(err)
	}
	scenarios := data["scenarios"].([]any)
	if len(scenarios) < 25 {
		t.Fatalf("only %d scenarios", len(scenarios))
	}
	for _, s := range scenarios {
		scenario := s.(obj)
		t.Run(name+"/"+scenario["name"].(string), func(t *testing.T) { runScenario(t, data, scenario, make(t)) })
	}
}

func merge(base, patch obj) obj {
	out := obj{}
	for k, v := range base {
		out[k] = v
	}
	for k, v := range patch {
		pm, pok := v.(obj)
		bm, bok := base[k].(obj)
		if pok && bok {
			out[k] = merge(bm, pm)
		} else {
			out[k] = v
		}
	}
	return out
}

func asObj(v any) obj {
	m, _ := v.(obj)
	return m
}

func str(v any) string { s, _ := v.(string); return s }

// roundTrip converts any Go value to its generic JSON form.
func roundTrip(t *testing.T, v any) any {
	t.Helper()
	raw, err := json.Marshal(v)
	if err != nil {
		t.Fatal(err)
	}
	var out any
	if err := json.Unmarshal(raw, &out); err != nil {
		t.Fatal(err)
	}
	return out
}

func matches(t *testing.T, actual, expected any, refs map[string]string, path string) {
	t.Helper()
	switch e := expected.(type) {
	case string:
		switch {
		case strings.HasPrefix(e, "$"):
			if actual != refs[e[1:]] {
				t.Errorf("%s: %v, want reference %s = %s", path, actual, e, refs[e[1:]])
			}
		case e == ">0":
			s, _ := actual.(string)
			if n, err := strconv.ParseUint(s, 10, 64); err != nil || n == 0 {
				t.Errorf("%s: %v is not > 0", path, actual)
			}
		default:
			if actual != e {
				t.Errorf("%s: %v, want %q", path, actual, e)
			}
		}
	case []any:
		a, ok := actual.([]any)
		if !ok || len(a) != len(e) {
			t.Errorf("%s: %v, want %d elements %v", path, actual, len(e), e)
			return
		}
		for i := range e {
			matches(t, a[i], e[i], refs, path+"["+strconv.Itoa(i)+"]")
		}
	case obj:
		a, ok := actual.(obj)
		if !ok {
			t.Errorf("%s: %v, want object", path, actual)
			return
		}
		for k, v := range e {
			matches(t, a[k], v, refs, path+"."+k)
		}
	default:
		if !reflect.DeepEqual(actual, expected) {
			t.Errorf("%s: %v, want %v", path, actual, expected)
		}
	}
}

func runScenario(t *testing.T, data, scenario obj, h *Harness) {
	ctx := context.Background()
	refs := map[string]string{}
	behaviors := data["providers"].(obj)
	if own, ok := scenario["providers"].(obj); ok {
		behaviors = own
	}
	mocks := map[string]*router.MockProvider{}
	providers := map[string]contracts.Provider{}
	for name, b := range behaviors {
		mocks[name] = router.NewMockProvider(str(b))
		providers[name] = mocks[name]
	}
	rt, err := router.New(h.Store, providers, DigestKey, router.WithClock(h.Clock))
	if err != nil {
		t.Fatal(err)
	}
	workspace := "ws-" + uuid.New()
	principals := map[string]contracts.Principal{}
	for name, raw := range data["principals"].(obj) {
		m := raw.(obj)
		p := contracts.Principal{Workspace: workspace, Environment: "test", Customer: str(m["customer"])}
		for _, s := range m["scopes"].([]any) {
			p.Scopes = append(p.Scopes, s.(string))
		}
		if ids, ok := m["service_ids"].([]any); ok {
			p.ServiceIDs = []string{}
			for _, id := range ids {
				p.ServiceIDs = append(p.ServiceIDs, id.(string))
			}
		}
		principals[name] = p
	}
	who := func(step obj) contracts.Principal {
		name := str(step["principal"])
		if name == "" {
			name = "owner"
		}
		return principals[name]
	}
	service := func(step obj) contracts.Service {
		doc := merge(asObj(data["services"].(obj)[str(step["service"])]), asObj(step["patch"]))
		raw, _ := json.Marshal(doc)
		s, err := contracts.ParseService(raw)
		if err != nil {
			t.Fatalf("service %v: %v", step["service"], err)
		}
		return s
	}
	for index, s := range scenario["steps"].([]any) {
		step := s.(obj)
		op := str(step["op"])
		label := scenario["name"].(string) + " #" + strconv.Itoa(index) + " " + op
		var result any
		var failure error
		switch op {
		case "publish":
			var mode string = str(step["mode"])
			_, failure = h.Store.Publish(ctx, who(step), service(step), mode, nil)
		case "grant":
			var expires *time.Time
			if ms, ok := step["expires_in_ms"].(float64); ok {
				e := h.Clock().Add(time.Duration(ms) * time.Millisecond)
				expires = &e
			}
			result, failure = h.Store.Grant(ctx, who(step), str(step["kind"]), str(step["asset"]), str(step["amount"]), str(step["source"]), expires)
		case "infer":
			doc := merge(asObj(data["invocation"]), asObj(step["input"]))
			raw, _ := json.Marshal(doc)
			key := str(step["key"])
			if key == "" {
				key = "key-" + strconv.Itoa(index)
			}
			var res *router.Result
			res, failure = rt.Infer(ctx, who(step), raw, key, nil)
			if failure == nil {
				result = roundTrip(t, res)
				if as := str(step["as"]); as != "" {
					refs[as] = res.ID
				}
			}
		case "balance":
			var rows []router.BalanceRow
			rows, failure = h.Store.Balance(ctx, who(step))
			result = roundTrip(t, rows)
		case "buckets":
			result = roundTrip(t, h.Buckets(who(step)))
		case "get":
			var r *router.RequestRecord
			r, failure = h.Store.Get(ctx, who(step), refs[str(step["request"])])
			if failure == nil {
				result = obj{"execution": r.Execution, "billing": r.Billing, "cancel_requested": r.CancelRequested, "output_started": r.OutputStarted, "provider_spent": r.ProviderSpent}
			}
		case "cancel":
			failure = h.Store.Cancel(ctx, who(step), refs[str(step["request"])])
		case "advance":
			h.Advance(int64(step["ms"].(float64)))
		case "disable":
			failure = h.Store.Disable(ctx, who(step), str(step["offering"]))
		case "recover":
			var n int
			n, failure = h.Store.Recover(ctx)
			result = float64(n)
		case "provider_calls":
			result = float64(mocks[str(step["provider"])].Calls())
		case "reserve":
			svc := service(step)
			p := who(step)
			var legs []router.Leg
			legs, failure = h.Store.Accounts(ctx, p, svc)
			if failure != nil {
				break
			}
			reserved := asObj(step["reserved"])
			for i := range legs {
				legs[i].Reserved = "0"
				if v, ok := reserved[legs[i].Kind]; ok {
					legs[i].Reserved = str(v)
				}
			}
			digest := str(step["digest"])
			if digest == "" {
				digest = "d"
			}
			deadline := h.Clock().Add(time.Duration(step["timeout_ms"].(float64)) * time.Millisecond)
			var out router.Reservation
			out, failure = h.Store.Reserve(ctx, p, str(step["key"]), digest, svc, legs, deadline, router.Decision{})
			if failure == nil {
				refs[str(step["as"])] = out.Request.ID
				result = obj{"fresh": out.Fresh, "execution": out.Request.Execution, "billing": out.Request.Billing}
			}
		case "begin":
			svc := service(step)
			var offering contracts.Offering
			for _, o := range svc.Offerings {
				if o.ID == str(step["offering"]) {
					offering = o
				}
			}
			var id string
			id, failure = h.Store.Begin(ctx, who(step), refs[str(step["request"])], offering, str(step["bound"]))
			if failure == nil {
				refs[str(step["as"])] = id
			}
		case "attempt":
			var usage *contracts.Meters
			if u, ok := step["usage"]; ok && u != nil {
				raw, _ := json.Marshal(u)
				usage = &contracts.Meters{}
				if err := json.Unmarshal(raw, usage); err != nil {
					t.Fatal(err)
				}
			}
			var cost *string
			if c, ok := step["cost"].(string); ok {
				cost = &c
			}
			failure = h.Store.AttemptResult(ctx, who(step), refs[str(step["request"])], refs[str(step["attempt"])], str(step["status"]), usage, cost, "")
		case "output":
			failure = h.Store.Output(ctx, who(step), refs[str(step["request"])])
		case "finish":
			var usage *contracts.Meters
			if u, ok := step["usage"]; ok && u != nil {
				raw, _ := json.Marshal(u)
				usage = &contracts.Meters{}
				if err := json.Unmarshal(raw, usage); err != nil {
					t.Fatal(err)
				}
			}
			pending, _ := step["pending"].(bool)
			failure = h.Store.Finish(ctx, who(step), refs[str(step["request"])], str(step["execution"]), usage, pending)
		default:
			t.Fatalf("%s: unknown op", label)
		}
		if want := asObj(step["error"]); want != nil {
			var re *contracts.RouterError
			if !errors.As(failure, &re) {
				t.Errorf("%s: error %v, want %v", label, failure, want["code"])
				continue
			}
			if re.Code != want["code"] {
				t.Errorf("%s: error %s, want %v", label, re.Code, want["code"])
			}
			if status, ok := want["status"].(float64); ok && re.Status != int(status) {
				t.Errorf("%s: status %d, want %v", label, re.Status, status)
			}
			continue
		}
		if failure != nil {
			t.Errorf("%s: unexpected error %v", label, failure)
			continue
		}
		if expect, ok := step["expect"]; ok {
			matches(t, result, expect, refs, label)
		}
	}
}
