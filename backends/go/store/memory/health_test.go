// SPDX-License-Identifier: Apache-2.0

package memory_test

import (
	"context"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"sync/atomic"
	"testing"
	"time"

	"github.com/Super-Solution/super-router/backends/go/contracts"
	"github.com/Super-Solution/super-router/backends/go/internal/uuid"
	"github.com/Super-Solution/super-router/backends/go/router"
	"github.com/Super-Solution/super-router/backends/go/routing"
	"github.com/Super-Solution/super-router/backends/go/store/memory"
)

// scripted answers every call the same way and counts the calls.
type scripted struct {
	err   error
	calls atomic.Int64
}

func (s *scripted) Invoke(_ context.Context, _ contracts.Invocation, _ contracts.Offering, emit func(contracts.ProviderEvent) error) error {
	s.calls.Add(1)
	if s.err != nil {
		return s.err
	}
	for _, e := range []contracts.ProviderEvent{
		{Type: "delta", Text: "ok"},
		{Type: "usage", Usage: contracts.Meters{Input: 10, Output: 5}, UpstreamID: "up"},
		{Type: "done"},
	} {
		if err := emit(e); err != nil {
			return err
		}
	}
	return nil
}

// twoOfferings publishes a service whose offerings a (provider "first") and b (provider "second") cost the same, so a
// is preferred, and returns a router over the two scripted providers. The clock can be moved with advance.
func twoOfferings(t *testing.T, first, second contracts.Provider, book *routing.HealthBook) (contracts.Principal, *router.Router, func(time.Duration)) {
	t.Helper()
	raw, err := os.ReadFile(filepath.Join("..", "..", "..", "rust", "conformance", "dataplane.v1.json"))
	if err != nil {
		t.Fatal(err)
	}
	var data struct {
		Services map[string]map[string]any `json:"services"`
	}
	if err := json.Unmarshal(raw, &data); err != nil {
		t.Fatal(err)
	}
	svc := data.Services["chat"]
	offerings := svc["offerings"].([]any)
	a := offerings[0].(map[string]any)
	a["provider"] = "first"
	b := map[string]any{}
	for k, v := range a {
		b[k] = v
	}
	b["id"], b["provider"] = "b", "second"
	svc["offerings"] = []any{a, b}
	encoded, _ := json.Marshal(svc)
	service, err := contracts.ParseService(encoded)
	if err != nil {
		t.Fatal(err)
	}
	var offset atomic.Int64
	clock := func() time.Time { return time.Now().Add(time.Duration(offset.Load())) }
	store := memory.New(clock)
	p := contracts.Principal{Workspace: "health-" + uuid.New(), Environment: "test", Customer: "c1",
		Scopes: []string{"invoke", "service:write", "billing:write", "billing:read"}}
	ctx := context.Background()
	if _, err := store.Publish(ctx, p, service, "self-host", nil); err != nil {
		t.Fatal(err)
	}
	if _, err := store.Grant(ctx, p, "customer_sale", "credit:demo", "100000", "g1", nil); err != nil {
		t.Fatal(err)
	}
	opts := []router.Option{router.WithClock(clock)}
	if book != nil {
		opts = append(opts, router.WithHealth(book))
	}
	rt, err := router.New(store, map[string]contracts.Provider{"first": first, "second": second}, "health-test-digest-key-0123456789abcdef", opts...)
	if err != nil {
		t.Fatal(err)
	}
	return p, rt, func(d time.Duration) { offset.Add(int64(d)) }
}

const chatBody = `{"service":"chat","customer_id":"c1","payer_mode":"developer","input":{"messages":[{"role":"user","content":"Hello"}]},"limits":{"max_output_tokens":32,"timeout_ms":30000}}`

func infer(t *testing.T, rt *router.Router, p contracts.Principal, key string) string {
	t.Helper()
	res, err := rt.Infer(context.Background(), p, []byte(chatBody), key, nil)
	if err != nil {
		t.Fatalf("%s: %v", key, err)
	}
	return res.ExecutionStatus
}

func calls(first, second *scripted) [2]int64 {
	return [2]int64{first.calls.Load(), second.calls.Load()}
}

func TestAnOfferingThatRefusesForItsOwnReasonsHandsTheRequestOnAndIsThenSkipped(t *testing.T) {
	first := &scripted{err: &contracts.ProviderError{Definitive: true, Retryable: true, Code: "credentials_rejected"}}
	second := &scripted{}
	p, rt, advance := twoOfferings(t, first, second, routing.NewHealthBook(routing.DefaultHealthPolicy()))
	steps := []struct {
		key   string
		after time.Duration
		want  [2]int64
		why   string
	}{
		{"k1", 0, [2]int64{1, 1}, "the first request pays one failed attempt on a, then b answers"},
		{"k2", 0, [2]int64{1, 2}, "within the cooldown a is not even tried"},
		{"k3", 5 * time.Minute, [2]int64{2, 3}, "after the cooldown one request probes a; it fails again"},
		{"k4", 5 * time.Minute, [2]int64{2, 4}, "the second cooldown is twice as long"},
		{"k5", 5 * time.Minute, [2]int64{3, 5}, "and a is probed again after it"},
	}
	for _, s := range steps {
		advance(s.after)
		if got := infer(t, rt, p, s.key); got != "succeeded" || calls(first, second) != s.want {
			t.Fatalf("%s: %s, calls %v, want %v (%s)", s.key, got, calls(first, second), s.want, s.why)
		}
	}
}

func TestTheWaitAProviderAskedForKeepsTheOfferingOutThatLong(t *testing.T) {
	// A rate limit that says "come back in 20 s": the default would reopen the offering after 5 s.
	first := &scripted{err: &contracts.ProviderError{Definitive: true, Retryable: true, Code: "rate_limited", RetryAfterMs: 20_000}}
	second := &scripted{}
	p, rt, advance := twoOfferings(t, first, second, routing.NewHealthBook(routing.DefaultHealthPolicy()))
	steps := []struct {
		key   string
		after time.Duration
		want  [2]int64
		why   string
	}{
		{"k1", 0, [2]int64{1, 1}, "the first request pays one failed attempt on a, then b answers"},
		{"k2", 19_999 * time.Millisecond, [2]int64{1, 2}, "still skipped one millisecond before the asked wait ends"},
		{"k3", time.Millisecond, [2]int64{2, 3}, "probed once the asked wait is over"},
	}
	for _, s := range steps {
		advance(s.after)
		if got := infer(t, rt, p, s.key); got != "succeeded" || calls(first, second) != s.want {
			t.Fatalf("%s: %s, calls %v, want %v (%s)", s.key, got, calls(first, second), s.want, s.why)
		}
	}
}

func TestWithoutAHealthBookEveryRequestPaysTheFailedAttempt(t *testing.T) {
	first := &scripted{err: &contracts.ProviderError{Definitive: true, Retryable: true, Code: "credentials_rejected"}}
	second := &scripted{}
	p, rt, _ := twoOfferings(t, first, second, nil)
	for _, key := range []string{"k1", "k2", "k3"} {
		infer(t, rt, p, key)
	}
	if got := calls(first, second); got != [2]int64{3, 3} {
		t.Fatalf("calls %v", got)
	}
}

func TestARefusedRequestOrAnUnknownOutcomeNeverCoolsAnOffering(t *testing.T) {
	for _, code := range []string{"upstream_http_422", "upstream_failed"} {
		first := &scripted{err: &contracts.ProviderError{Definitive: true, Retryable: true, Code: code}}
		second := &scripted{}
		book := routing.NewHealthBook(routing.DefaultHealthPolicy())
		p, rt, _ := twoOfferings(t, first, second, book)
		infer(t, rt, p, "k1")
		infer(t, rt, p, "k2")
		if first.calls.Load() != 2 || book.Len() != 0 {
			t.Errorf("%s: a was called %d times, %d records", code, first.calls.Load(), book.Len())
		}
	}
}

func TestWhenEveryOfferingIsCoolingNoneIsSkipped(t *testing.T) {
	refusing := func() *scripted {
		return &scripted{err: &contracts.ProviderError{Definitive: true, Retryable: true, Code: "rate_limited"}}
	}
	first, second := refusing(), refusing()
	p, rt, _ := twoOfferings(t, first, second, routing.NewHealthBook(routing.DefaultHealthPolicy()))
	infer(t, rt, p, "k1")
	if got := calls(first, second); got != [2]int64{1, 1} {
		t.Fatalf("calls %v", got)
	}
	// Both are cooling now; a request that might fail beats one that certainly does.
	infer(t, rt, p, "k2")
	if got := calls(first, second); got != [2]int64{2, 2} {
		t.Fatalf("calls %v", got)
	}
}

// finishing answers every call with a partial answer that ends with the given completion.
type finishing struct{ completion string }

func (f finishing) Invoke(_ context.Context, _ contracts.Invocation, _ contracts.Offering, emit func(contracts.ProviderEvent) error) error {
	for _, e := range []contracts.ProviderEvent{
		{Type: "delta", Text: "x"},
		{Type: "usage", Usage: contracts.Meters{Input: 10, Output: 1}, UpstreamID: "up"},
		{Type: "done", Completion: f.completion},
	} {
		if err := emit(e); err != nil {
			return err
		}
	}
	return nil
}

func TestAnAnswerCutShortSaysWhyInItsFinishReason(t *testing.T) {
	for completion, finish := range map[string]string{"filtered": "content_filter", "partial": "length"} {
		p, rt, _ := twoOfferings(t, finishing{completion}, finishing{completion}, nil)
		var events []contracts.StreamEvent
		res, err := rt.Infer(context.Background(), p, []byte(chatBody), "k1", func(e contracts.StreamEvent) error {
			events = append(events, e)
			return nil
		})
		// Either way the customer got a partial answer and the books treat it as one.
		if err != nil || res.ExecutionStatus != "partial" || res.FinishReason != finish {
			t.Fatalf("%s: %v %s %q, want partial %s", completion, err, res.ExecutionStatus, res.FinishReason, finish)
		}
		last := events[len(events)-1]
		if last.Type != "request.failed" || last.Data["finish_reason"] != finish {
			t.Errorf("%s: last event %s %v", completion, last.Type, last.Data)
		}
	}
}

// structured says whether it carries structured output, and records the schema names it was sent.
type structured struct {
	carries bool
	seen    []string
}

func (s *structured) Capabilities() contracts.ProviderCapabilities {
	return contracts.ProviderCapabilities{StructuredOutput: s.carries}
}

func (s *structured) Invoke(ctx context.Context, in contracts.Invocation, o contracts.Offering, emit func(contracts.ProviderEvent) error) error {
	if f := in.ResponseFormat; f != nil {
		s.seen = append(s.seen, f.JSONSchema.Name)
	}
	return (&scripted{}).Invoke(ctx, in, o, emit)
}

func TestStructuredOutputReachesOnlyAProviderThatCarriesIt(t *testing.T) {
	body := `{"service":"chat","customer_id":"c1","payer_mode":"developer","input":{"messages":[{"role":"user","content":"Hello"}]},"limits":{"max_output_tokens":32,"timeout_ms":30000},` +
		`"response_format":{"type":"json_schema","json_schema":{"name":"answer","schema":{"type":"object"}}}}`
	for _, carries := range []bool{true, false} {
		raw, err := os.ReadFile(filepath.Join("..", "..", "..", "rust", "conformance", "dataplane.v1.json"))
		if err != nil {
			t.Fatal(err)
		}
		var data struct {
			Services map[string]map[string]any `json:"services"`
		}
		if err := json.Unmarshal(raw, &data); err != nil {
			t.Fatal(err)
		}
		svc := data.Services["chat"]
		svc["offerings"].([]any)[0].(map[string]any)["capabilities"] = []any{"structured_output"}
		encoded, _ := json.Marshal(svc)
		service, err := contracts.ParseService(encoded)
		if err != nil {
			t.Fatal(err)
		}
		store := memory.New(time.Now)
		p := contracts.Principal{Workspace: "structured-" + uuid.New(), Environment: "test", Customer: "c1",
			Scopes: []string{"invoke", "service:write", "billing:write", "billing:read"}}
		ctx := context.Background()
		if _, err := store.Publish(ctx, p, service, "self-host", nil); err != nil {
			t.Fatal(err)
		}
		if _, err := store.Grant(ctx, p, "customer_sale", "credit:demo", "100000", "g1", nil); err != nil {
			t.Fatal(err)
		}
		provider := &structured{carries: carries}
		rt, err := router.New(store, map[string]contracts.Provider{"ok": provider}, "health-test-digest-key-0123456789abcdef")
		if err != nil {
			t.Fatal(err)
		}
		res, err := rt.Infer(ctx, p, []byte(body), "k1", nil)
		if carries {
			if err != nil || res.ExecutionStatus != "succeeded" || len(provider.seen) != 1 || provider.seen[0] != "answer" {
				t.Fatalf("carrying: %v %+v %v", err, res, provider.seen)
			}
			continue
		}
		var re *contracts.RouterError
		if !errors.As(err, &re) || re.Code != "unsupported_capability" || len(provider.seen) != 0 {
			t.Fatalf("not carrying: %v %v", err, provider.seen)
		}
		rows, err := store.Balance(ctx, p)
		if err != nil || rows[0].Held != "0" || rows[0].Balance != "100000" {
			t.Fatalf("nothing may be reserved: %v %+v", err, rows)
		}
	}
}
