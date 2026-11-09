// SPDX-License-Identifier: Apache-2.0

package server_test

import (
	"bufio"
	"context"
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/Super-Solution/super-router/backends/go/contracts"
	"github.com/Super-Solution/super-router/backends/go/router"
	"github.com/Super-Solution/super-router/backends/go/server"
	"github.com/Super-Solution/super-router/backends/go/store/memory"
)

const digestKey = "conformance-digest-key-0123456789abcdef"

var owner = contracts.Principal{
	Workspace: "ws", Environment: "test", Customer: "c1",
	Scopes: []string{"invoke", "service:write", "billing:write", "billing:read", "routing:write"},
}

type fixture struct {
	srv   *httptest.Server
	store *memory.Store
	mock  *router.MockProvider
}

func setup(t *testing.T, behavior string, delay time.Duration) *fixture {
	t.Helper()
	raw, err := os.ReadFile(filepath.Join("..", "..", "..", "conformance", "dataplane.v1.json"))
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
	store := memory.New(nil)
	ctx := context.Background()
	if _, err := store.Publish(ctx, owner, service, "", nil); err != nil {
		t.Fatal(err)
	}
	if _, err := store.Grant(ctx, owner, "customer_sale", "credit:demo", "100000", "g1", nil); err != nil {
		t.Fatal(err)
	}
	mock := router.NewMockProvider(behavior)
	mock.Delay = delay
	rt, err := router.New(store, map[string]contracts.Provider{"ok": mock}, digestKey)
	if err != nil {
		t.Fatal(err)
	}
	reader := contracts.Principal{Workspace: "ws", Environment: "test", Customer: "c1", Scopes: []string{"billing:read"}}
	handler, err := server.Handler(server.Options{Router: rt, Store: store, Auth: server.StaticKeys([]server.Key{
		{Secret: "owner-key", Principal: owner}, {Secret: "reader-key", Principal: reader},
	})})
	if err != nil {
		t.Fatal(err)
	}
	srv := httptest.NewServer(handler)
	t.Cleanup(srv.Close)
	return &fixture{srv, store, mock}
}

const body = `{"service":"chat","customer_id":"c1","input":{"messages":[{"role":"user","content":"Hello"}]},"limits":{"max_output_tokens":32}%s}`

func (f *fixture) do(t *testing.T, method, path, key, idem, payload string) *http.Response {
	t.Helper()
	req, err := http.NewRequest(method, f.srv.URL+path, strings.NewReader(payload))
	if err != nil {
		t.Fatal(err)
	}
	if key != "" {
		req.Header.Set("Authorization", "Bearer "+key)
	}
	if idem != "" {
		req.Header.Set("Idempotency-Key", idem)
	}
	client := &http.Client{CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }}
	res, err := client.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { res.Body.Close() })
	return res
}

func decode(t *testing.T, res *http.Response) map[string]any {
	t.Helper()
	var out map[string]any
	if err := json.NewDecoder(res.Body).Decode(&out); err != nil {
		t.Fatal(err)
	}
	return out
}

func errorCode(t *testing.T, res *http.Response) string {
	t.Helper()
	e, _ := decode(t, res)["error"].(map[string]any)
	code, _ := e["code"].(string)
	return code
}

func TestInferenceSettles(t *testing.T) {
	f := setup(t, "success", 0)
	res := f.do(t, "POST", "/v1/inferences", "owner-key", "k1", strings.Replace(body, "%s", "", 1))
	if res.StatusCode != 200 || res.Header.Get("X-Request-Id") == "" {
		t.Fatalf("status %d", res.StatusCode)
	}
	out := decode(t, res)
	if out["billing_status"] != "settled" || out["execution_status"] != "succeeded" {
		t.Fatalf("%v", out)
	}
	replay := f.do(t, "POST", "/v1/inferences", "owner-key", "k1", strings.Replace(body, "%s", "", 1))
	if replay.StatusCode != 303 || replay.Header.Get("Location") != "/v1/requests/"+out["id"].(string) {
		t.Fatalf("replay %d %q", replay.StatusCode, replay.Header.Get("Location"))
	}
	if f.mock.Calls() != 1 {
		t.Fatalf("provider called %d times", f.mock.Calls())
	}
	status := f.do(t, "GET", "/v1/requests/"+out["id"].(string), "owner-key", "", "")
	if got := decode(t, status); got["billing_status"] != "settled" {
		t.Fatalf("%v", got)
	}
	balance := decode(t, f.do(t, "GET", "/v1/admin/balance", "owner-key", "", ""))
	rows := balance["data"].([]any)
	if len(rows) != 1 || rows[0].(map[string]any)["balance"] != "99965" {
		t.Fatalf("%v", balance)
	}
}

func TestErrors(t *testing.T) {
	f := setup(t, "success", 0)
	payload := strings.Replace(body, "%s", "", 1)
	cases := []struct {
		name                           string
		method, path, key, idem, input string
		status                         int
		code                           string
	}{
		{"no key", "POST", "/v1/inferences", "", "k", payload, 401, "invalid_key"},
		{"wrong key", "POST", "/v1/inferences", "nope", "k", payload, 401, "invalid_key"},
		{"no idempotency key", "POST", "/v1/inferences", "owner-key", "", payload, 400, "missing_idempotency_key"},
		{"not json", "POST", "/v1/inferences", "owner-key", "k", "{", 400, "invalid_json"},
		{"unknown field", "POST", "/v1/inferences", "owner-key", "k", strings.Replace(body, "%s", `,"extra":1`, 1), 400, "invalid_request"},
		{"scope", "POST", "/v1/inferences", "reader-key", "k", payload, 403, "scope_denied"},
		{"other customer", "POST", "/v1/inferences", "owner-key", "k", strings.Replace(payload, `"c1"`, `"c2"`, 1), 403, "scope_denied"},
		{"unknown service", "POST", "/v1/inferences", "owner-key", "k", strings.Replace(payload, `"chat"`, `"nope"`, 1), 404, "resource_not_found"},
		{"unknown route", "GET", "/v1/nothing", "owner-key", "", "", 404, "resource_not_found"},
		{"wrong method", "DELETE", "/v1/services", "owner-key", "", "", 404, "resource_not_found"},
		{"reader cannot list", "GET", "/v1/services", "reader-key", "", "", 403, "scope_denied"},
		{"balance scope", "GET", "/v1/admin/balance", "reader-key", "", "", 200, ""},
		{"unknown request", "GET", "/v1/requests/nope", "owner-key", "", "", 404, "resource_not_found"},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			res := f.do(t, c.method, c.path, c.key, c.idem, c.input)
			if res.StatusCode != c.status {
				t.Fatalf("status %d, want %d", res.StatusCode, c.status)
			}
			if c.code != "" {
				if got := errorCode(t, res); got != c.code {
					t.Fatalf("code %q, want %q", got, c.code)
				}
			}
		})
	}
	if f.mock.Calls() != 0 {
		t.Fatalf("rejected requests reached the provider %d times", f.mock.Calls())
	}
}

func TestBodyLimit(t *testing.T) {
	f := setup(t, "success", 0)
	huge := `{"x":"` + strings.Repeat("a", server.MaxBodyBytes) + `"}`
	res := f.do(t, "POST", "/v1/inferences", "owner-key", "k", huge)
	if res.StatusCode != 413 || errorCode(t, res) != "body_too_large" {
		t.Fatalf("status %d", res.StatusCode)
	}
}

func TestStreaming(t *testing.T) {
	f := setup(t, "success", 0)
	res := f.do(t, "POST", "/v1/inferences", "owner-key", "s1", strings.Replace(body, "%s", `,"stream":true`, 1))
	if res.StatusCode != 200 || res.Header.Get("Content-Type") != "text/event-stream" {
		t.Fatalf("status %d type %q", res.StatusCode, res.Header.Get("Content-Type"))
	}
	var kinds []string
	scanner := bufio.NewScanner(res.Body)
	for scanner.Scan() {
		if line, ok := strings.CutPrefix(scanner.Text(), "event: "); ok {
			kinds = append(kinds, line)
		}
	}
	want := []string{"request.started", "output.delta", "request.completed"}
	if strings.Join(kinds, ",") != strings.Join(want, ",") {
		t.Fatalf("events %v, want %v", kinds, want)
	}
	// A failure before the request exists is still a JSON error with a real status.
	bad := f.do(t, "POST", "/v1/inferences", "owner-key", "s2", strings.Replace(strings.Replace(body, "%s", `,"stream":true`, 1), `"chat"`, `"nope"`, 1))
	if bad.StatusCode != 404 || !strings.HasPrefix(bad.Header.Get("Content-Type"), "application/json") {
		t.Fatalf("status %d type %q", bad.StatusCode, bad.Header.Get("Content-Type"))
	}
}

func TestCancelAbortsTheProvider(t *testing.T) {
	f := setup(t, "success", 5*time.Second)
	type outcome struct{ out map[string]any }
	done := make(chan outcome, 1)
	go func() {
		res := f.do(t, "POST", "/v1/inferences", "owner-key", "c1", strings.Replace(body, "%s", "", 1))
		done <- outcome{decode(t, res)}
	}()
	deadline := time.Now().Add(3 * time.Second)
	for len(f.store.RequestIDs(owner)) == 0 && time.Now().Before(deadline) {
		time.Sleep(20 * time.Millisecond)
	}
	rec := findRequest(t, f)
	cancel := f.do(t, "POST", "/v1/requests/"+rec+"/cancel", "owner-key", "x", "{}")
	if cancel.StatusCode != 202 {
		t.Fatalf("cancel status %d", cancel.StatusCode)
	}
	select {
	case o := <-done:
		// The provider reported no result, so the outcome is unknown and the funds stay held for reconciliation.
		if o.out["error"] == nil && o.out["billing_status"] == "settled" {
			t.Fatalf("a cancelled request must not settle: %v", o.out)
		}
	case <-time.After(3 * time.Second):
		t.Fatal("cancel did not stop the provider")
	}
}

func findRequest(t *testing.T, f *fixture) string {
	t.Helper()
	id := f.store.RequestIDs(owner)
	if len(id) != 1 {
		t.Fatalf("requests: %v", id)
	}
	return id[0]
}

func TestClientDisconnectKeepsFundsHeld(t *testing.T) {
	f := setup(t, "success", 5*time.Second)
	ctx, cancel := context.WithCancel(context.Background())
	req, _ := http.NewRequestWithContext(ctx, "POST", f.srv.URL+"/v1/inferences", strings.NewReader(strings.Replace(body, "%s", "", 1)))
	req.Header.Set("Authorization", "Bearer owner-key")
	req.Header.Set("Idempotency-Key", "d1")
	go func() {
		res, err := http.DefaultClient.Do(req)
		if err == nil {
			io.Copy(io.Discard, res.Body)
			res.Body.Close()
		}
	}()
	deadline := time.Now().Add(3 * time.Second)
	for len(f.store.RequestIDs(owner)) == 0 && time.Now().Before(deadline) {
		time.Sleep(20 * time.Millisecond)
	}
	cancel()
	id := findRequest(t, f)
	for time.Now().Before(deadline) {
		rec, _ := f.store.Get(context.Background(), owner, id)
		if rec.Billing == "pending" {
			return
		}
		time.Sleep(20 * time.Millisecond)
	}
	t.Fatal("a dropped client must leave the reservation pending, never released")
}

func TestPublishAndDryRun(t *testing.T) {
	f := setup(t, "success", 0)
	payload := strings.Replace(body, "%s", "", 1)
	res := f.do(t, "POST", "/v1/admin/routing/dry-run", "owner-key", "r1", payload)
	if res.StatusCode != 200 {
		t.Fatalf("status %d", res.StatusCode)
	}
	out := decode(t, res)
	if c := out["candidates"].([]any); len(c) != 1 || c[0].(map[string]any)["id"] != "a" {
		t.Fatalf("%v", out)
	}
	if res := f.do(t, "POST", "/v1/admin/service-versions", "owner-key", "p1", `{"id":"x"}`); res.StatusCode != 400 || errorCode(t, res) != "invalid_request" {
		t.Fatalf("status %d", res.StatusCode)
	}
	if res := f.do(t, "GET", "/healthz", "", "", ""); res.StatusCode != 200 {
		t.Fatalf("healthz %d", res.StatusCode)
	}
}

func TestGrantEndpoint(t *testing.T) {
	f := setup(t, "success", 0)
	grant := `{"asset":"credit:demo","amount":"500"}`
	if res := f.do(t, "POST", "/v1/admin/credits/grants", "owner-key", "g-http", grant); res.StatusCode != 201 {
		t.Fatalf("status %d", res.StatusCode)
	}
	// The same Idempotency-Key with the same body does not fund twice; a different body conflicts.
	if res := f.do(t, "POST", "/v1/admin/credits/grants", "owner-key", "g-http", grant); res.StatusCode != 201 {
		t.Fatalf("replay status %d", res.StatusCode)
	}
	if res := f.do(t, "POST", "/v1/admin/credits/grants", "owner-key", "g-http", `{"asset":"credit:demo","amount":"501"}`); res.StatusCode != 409 {
		t.Fatalf("conflict status %d", res.StatusCode)
	}
	rows := decode(t, f.do(t, "GET", "/v1/admin/balance", "owner-key", "", ""))["data"].([]any)
	if rows[0].(map[string]any)["balance"] != "100500" {
		t.Fatalf("%v", rows)
	}
	for _, bad := range []string{`{}`, `{"asset":"credit:demo","amount":"0"}`, `{"asset":"credit:demo","amount":"5","extra":1}`, `{"asset":"credit:demo","amount":"5","kind":"x"}`} {
		if res := f.do(t, "POST", "/v1/admin/credits/grants", "owner-key", "g-bad", bad); res.StatusCode != 400 {
			t.Errorf("%s: status %d", bad, res.StatusCode)
		}
	}
	if res := f.do(t, "POST", "/v1/admin/credits/grants", "reader-key", "g-r", grant); res.StatusCode != 403 {
		t.Fatalf("scope status %d", res.StatusCode)
	}
}

func TestCreditRatesEndpoint(t *testing.T) {
	f := setup(t, "success", 0)
	units := func(n string) string { return `{"units":"` + n + `"}` }
	config := `{"asset":"credit:demo","tokens":{"input":` + units("1000") + `,"cached":` + units("1000") + `,"cache_write":` + units("1000") + `,"output":` + units("1000") + `}}`
	post := func(idem, body string) *http.Response {
		return f.do(t, "POST", "/v1/admin/services/chat/credit-rates", "owner-key", idem, body)
	}
	res := post("cr1", `{"expected_version":1,"config":`+config+`}`)
	if res.StatusCode != 201 {
		t.Fatalf("status %d", res.StatusCode)
	}
	if v := decode(t, res)["version"]; v != float64(2) {
		t.Fatalf("version %v", v)
	}
	if res := post("cr2", `{"expected_version":1,"config":`+config+`}`); res.StatusCode != 409 || errorCode(t, res) != "service_version_conflict" {
		t.Fatalf("stale version: %d", res.StatusCode)
	}
	for body, code := range map[string]string{
		`{"expected_version":"2"}`:                         "invalid_service_version",
		`{"expected_version":2.5,"config":` + config + `}`: "invalid_service_version",
		`{"x":1}`: "invalid_credit_rates",
		`{"expected_version":2,"config":{"asset":"credit:demo"}}`:            "invalid_credit_rates",
		`{"expected_version":2,"config":{"asset":"other","tokens":` + `{}}}`: "invalid_credit_rates",
	} {
		if res := post("cr-bad", body); errorCode(t, res) != code {
			t.Errorf("%s: %d, want %s", body, res.StatusCode, code)
		}
	}
	if res := f.do(t, "POST", "/v1/admin/services/chat/credit-rates", "reader-key", "cr3", `{"expected_version":2,"config":`+config+`}`); res.StatusCode != 403 {
		t.Fatalf("scope status %d", res.StatusCode)
	}
}

func TestStaticKeysRefuseAnEmptyToken(t *testing.T) {
	auth := server.StaticKeys([]server.Key{{Secret: "", Principal: owner}})
	if _, err := auth.Authenticate(context.Background(), ""); err == nil {
		t.Fatal("an empty secret must not authenticate a request that sent no credentials")
	}
}
