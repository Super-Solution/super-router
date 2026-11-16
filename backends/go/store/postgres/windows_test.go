// SPDX-License-Identifier: Apache-2.0

package postgres_test

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
	"github.com/Super-Solution/super-router/backends/go/store/postgres"
)

// One chat call reserves 2144 atoms and the mock settles it at 35. "four" leaves room for exactly four
// reservations: 35k + 2144 <= 2250 holds for k = 0..3.
const (
	charge = 35
	four   = "2250"
)

type windowFixture struct {
	t     *testing.T
	store *postgres.Store
	p     contracts.Principal
	rt    *router.Router
	input json.RawMessage
	calls int
}

// newWindowFixture publishes the conformance chat service, funds the customer through a subscription bucket
// (100000 credits) whose entitlement carries limits (nil: the option is off) and returns a router over a mock.
func newWindowFixture(t *testing.T, limits string, behavior string) *windowFixture {
	t.Helper()
	store := open(t)
	raw, err := os.ReadFile(filepath.Join("..", "..", "..", "..", "conformance", "dataplane.v1.json"))
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
	p := contracts.Principal{Workspace: "win-" + uuid.New(), Environment: "test", Customer: "c1",
		Scopes: []string{"invoke", "service:write", "billing:write", "billing:read", "credits:read"}}
	ctx := context.Background()
	if _, err := store.Publish(ctx, p, service, "", nil); err != nil {
		t.Fatal(err)
	}
	db := store.DB()
	for _, q := range []string{
		"INSERT INTO sr_customers(workspace,environment,id,external_id) VALUES($1,$2,'c1','c1')",
		"INSERT INTO sr_plans(workspace,environment,id,version,config) VALUES($1,$2,'plan',1,'{}')",
		"INSERT INTO sr_subscriptions(workspace,environment,id,customer,plan_id,plan_version,anchor_at,period_days,status) VALUES($1,$2,'sub','c1','plan',1,now(),30,'active')",
	} {
		if _, err := db.ExecContext(ctx, q, p.Workspace, p.Environment); err != nil {
			t.Fatal(err)
		}
	}
	expires := time.Now().Add(30 * 24 * time.Hour)
	if _, err := store.GrantWith(ctx, p, "customer_sale", "credit:demo", "100000", "subscription:sub:0",
		postgres.GrantOptions{Expires: &expires, ServiceIDs: []string{"chat"}, SourceKind: "subscription"}); err != nil {
		t.Fatal(err)
	}
	var bucket string
	if err := db.QueryRowContext(ctx, "SELECT id FROM sr_buckets WHERE workspace=$1 AND environment=$2 AND source='subscription:sub:0'", p.Workspace, p.Environment).Scan(&bucket); err != nil {
		t.Fatal(err)
	}
	var snapshot json.RawMessage
	if limits != "" {
		snapshot = json.RawMessage(limits)
	}
	if err := store.EntitleWith(ctx, p, "sub", 0, []string{"chat"}, time.Now().Add(-time.Hour), expires, bucket, snapshot); err != nil {
		t.Fatal(err)
	}
	rt, err := router.New(store, map[string]contracts.Provider{"ok": router.NewMockProvider(behavior)}, "window-digest-key-0123456789abcdef")
	if err != nil {
		t.Fatal(err)
	}
	return &windowFixture{t: t, store: store, p: p, rt: rt, input: data.Invocation}
}

func (f *windowFixture) call() (*router.Result, error) {
	f.calls++
	return f.rt.Infer(context.Background(), f.p, f.input, "k-"+strconv.Itoa(f.calls)+"-"+uuid.New(), nil)
}

func (f *windowFixture) mustCall() {
	f.t.Helper()
	if _, err := f.call(); err != nil {
		f.t.Fatalf("call %d: %v", f.calls, err)
	}
}

func (f *windowFixture) refused() *contracts.RouterError {
	f.t.Helper()
	_, err := f.call()
	var re *contracts.RouterError
	if !errors.As(err, &re) {
		f.t.Fatalf("expected a refusal, got %v", err)
	}
	return re
}

func (f *windowFixture) rows() int {
	var n int
	if err := f.store.DB().QueryRow("SELECT count(*) FROM sr_usage_windows WHERE workspace=$1", f.p.Workspace).Scan(&n); err != nil {
		f.t.Fatal(err)
	}
	return n
}

func (f *windowFixture) window(kind string) postgres.WindowRow {
	f.t.Helper()
	rows, err := f.store.Windows(context.Background(), f.p)
	if err != nil {
		f.t.Fatal(err)
	}
	for _, r := range rows {
		if r.Window == kind {
			return r
		}
	}
	f.t.Fatalf("no %s window in %v", kind, rows)
	return postgres.WindowRow{}
}

// age moves every window of a kind into the past, as if that much time had gone by.
func (f *windowFixture) age(kind string, hours int) {
	f.t.Helper()
	if _, err := f.store.DB().Exec("UPDATE sr_usage_windows SET window_start=window_start-make_interval(hours=>$3),window_end=window_end-make_interval(hours=>$3) WHERE workspace=$1 AND kind=$2", f.p.Workspace, kind, hours); err != nil {
		f.t.Fatal(err)
	}
}

func TestWindowsOffByDefault(t *testing.T) {
	for _, limits := range []string{"", `{"enabled":false,"session":{"hours":5,"credits":"1"}}`} {
		f := newWindowFixture(t, limits, "success")
		for i := 0; i < 3; i++ {
			f.mustCall()
		}
		if f.rows() != 0 {
			t.Fatalf("limits %q wrote windows", limits)
		}
	}
}

func TestSessionWindowAdmitsWhatFitsThenRefuses(t *testing.T) {
	f := newWindowFixture(t, `{"enabled":true,"session":{"hours":5,"credits":"`+four+`"}}`, "success")
	for i := 0; i < 4; i++ {
		f.mustCall()
	}
	w := f.window("session")
	if w.Spent != strconv.Itoa(charge*4) || w.Held != "0" || w.Limit != four || w.Hours != 5 {
		t.Fatalf("window after four calls: %+v", w)
	}
	if got := w.ResetsAt.Sub(*w.WindowStart); got != 5*time.Hour {
		t.Fatalf("window length %v", got)
	}
	re := f.refused()
	if re.Code != "window_limit_reached" || re.Status != 429 || !re.Retryable {
		t.Fatalf("refusal: %+v", re)
	}
	if re.Details["window"] != "session" || re.Details["limit"] != four || re.Details["used"] != strconv.Itoa(charge*4) {
		t.Fatalf("details: %v", re.Details)
	}
	// The refusal reserved nothing.
	var held string
	if err := f.store.DB().QueryRow("SELECT held::text FROM sr_accounts WHERE workspace=$1", f.p.Workspace).Scan(&held); err != nil || held != "0" {
		t.Fatalf("held %q %v", held, err)
	}
	// A new window opens at the first request after the old one closed.
	f.age("session", 6)
	f.mustCall()
	if got := f.window("session").Spent; got != strconv.Itoa(charge) {
		t.Fatalf("new window spent %s", got)
	}
}

func TestWeeklyWindowBindsIndependently(t *testing.T) {
	// The session fits four calls; the week fits eight (35k + 2144 <= 2400 for k = 0..7).
	f := newWindowFixture(t, `{"enabled":true,"session":{"hours":5,"credits":"`+four+`"},"weekly":{"credits":"2400"}}`, "success")
	for i := 0; i < 4; i++ {
		f.mustCall()
	}
	if re := f.refused(); re.Details["window"] != "session" {
		t.Fatalf("%v", re.Details)
	}
	f.age("session", 6)
	for i := 0; i < 4; i++ {
		f.mustCall()
	}
	if re := f.refused(); re.Details["window"] != "session" && re.Details["window"] != "weekly" {
		t.Fatalf("%v", re.Details)
	}
	f.age("session", 6)
	if re := f.refused(); re.Details["window"] != "weekly" {
		t.Fatalf("expected the week to bind: %v", re.Details)
	}
	f.age("weekly", 169)
	f.mustCall()
}

func TestLimitBelowOneReservationIsNotRetryable(t *testing.T) {
	f := newWindowFixture(t, `{"enabled":true,"session":{"hours":5,"credits":"1000"}}`, "success")
	re := f.refused()
	if re.Code != "window_limit_reached" || re.Retryable || re.Details["resets_at"] != nil {
		t.Fatalf("%+v", re)
	}
	if f.rows() != 0 {
		t.Fatal("a refused request opened a window")
	}
}

func TestFullWindowFallsThroughToPurchasedCredits(t *testing.T) {
	f := newWindowFixture(t, `{"enabled":true,"session":{"hours":5,"credits":"`+four+`"}}`, "success")
	if _, err := f.store.Grant(context.Background(), f.p, "customer_sale", "credit:demo", "100000", "pack", nil); err != nil {
		t.Fatal(err)
	}
	// 75 calls cost 2625: the window takes exactly its 2250 and the pack the other 375, including the call whose
	// hold straddles both buckets.
	for i := 0; i < 75; i++ {
		f.mustCall()
	}
	if w := f.window("session"); w.Spent != "2250" || w.Held != "0" {
		t.Fatalf("%+v", w)
	}
	buckets, err := f.store.Buckets(context.Background(), f.p)
	if err != nil {
		t.Fatal(err)
	}
	for _, b := range buckets {
		want := "97750"
		if b.Source == "pack" {
			want = "99625"
		}
		if b.Remaining != want || b.Held != "0" {
			t.Fatalf("%s: %+v", b.Source, b)
		}
	}
}

func TestReleasedRequestDoesNotStartTheClock(t *testing.T) {
	f := newWindowFixture(t, `{"enabled":true,"session":{"hours":5,"credits":"`+four+`"}}`, "reject")
	if res, err := f.call(); err == nil && res.BillingStatus != "released" {
		t.Fatalf("billing %s", res.BillingStatus)
	}
	if f.rows() != 0 {
		t.Fatal("a released request left a window behind")
	}
}

func TestConcurrentCallsCannotOvershootAWindow(t *testing.T) {
	f := newWindowFixture(t, `{"enabled":true,"session":{"hours":5,"credits":"`+four+`"}}`, "success")
	var wg sync.WaitGroup
	results := make([]error, 12)
	for i := range results {
		wg.Add(1)
		go func(i int) {
			defer wg.Done()
			_, results[i] = f.rt.Infer(context.Background(), f.p, f.input, "c-"+strconv.Itoa(i), nil)
		}(i)
	}
	wg.Wait()
	ok := 0
	for _, err := range results {
		if err == nil {
			ok++
			continue
		}
		var re *contracts.RouterError
		if !errors.As(err, &re) || re.Code != "window_limit_reached" {
			t.Fatalf("unexpected failure: %v", err)
		}
	}
	if ok < 1 || ok > 4 {
		t.Fatalf("admitted %d", ok)
	}
	if w := f.window("session"); w.Spent != strconv.Itoa(charge*ok) || w.Held != "0" {
		t.Fatalf("%d admitted, window %+v", ok, w)
	}
}
