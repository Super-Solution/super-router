// SPDX-License-Identifier: Apache-2.0

package server_test

import (
	"bufio"
	"context"
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/Super-Solution/super-router/backends/go/contracts"
	"github.com/Super-Solution/super-router/backends/go/internal/uuid"
	"github.com/Super-Solution/super-router/backends/go/router"
	"github.com/Super-Solution/super-router/backends/go/server"
	"github.com/Super-Solution/super-router/backends/go/store/memory"
)

const lookup = `{"name":"lookup","description":"Find a thing","input_schema":{"type":"object","properties":{"q":{"type":"string"}}}}`

func withTools(extra string) string {
	return `{"service":"chat","customer_id":"c1","input":{"messages":[{"role":"user","content":"Find it"}],"tools":[` + lookup + `]` +
		extra + `},"limits":{"max_output_tokens":32}}`
}

func streaming(body string) string { return strings.TrimSuffix(body, "}") + `,"stream":true}` }

type toolFixture struct {
	srv   *httptest.Server
	store *memory.Store
}

const offeringJSON = `{"id":"%s","provider":"%s","model":"mock-model","upstream_model":"mock-model","region":"test","labels":[],"enabled":true,"source":"byok","max_input_tokens":1024,"max_output_tokens":128,"price":{"asset":"USD","input":"1","cached":"1","cache_write":"1","output":"1","per":1}}`

func toolSetup(t *testing.T, providers map[string]contracts.Provider, offerings ...string) *toolFixture {
	t.Helper()
	if len(offerings) == 0 {
		offerings = []string{strings.Replace(strings.Replace(offeringJSON, "%s", "a", 1), "%s", "mock", 1)}
	}
	service, err := contracts.ParseService([]byte(`{"id":"chat","version":1,"mode":"pool","regions":["test"],"price":{"asset":"credit:demo","input":"2","cached":"1","cache_write":"2","output":"3"},` +
		`"max_attempts":2,"max_provider_cost":"10000","max_output_tokens":128,"weights":{"cost":1,"latency":0,"reliability":0,"quality":0},"cost_scale":"10000","latency_scale_ms":1000,` +
		`"offerings":[` + strings.Join(offerings, ",") + `]}`))
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
	rt, err := router.New(store, providers, digestKey)
	if err != nil {
		t.Fatal(err)
	}
	handler, err := server.Handler(server.Options{Router: rt, Store: store, Auth: server.StaticKeys([]server.Key{{Secret: "owner-key", Principal: owner}})})
	if err != nil {
		t.Fatal(err)
	}
	srv := httptest.NewServer(handler)
	t.Cleanup(srv.Close)
	return &toolFixture{srv, store}
}

func offering(id, provider string) string {
	return strings.Replace(strings.Replace(offeringJSON, "%s", id, 1), "%s", provider, 1)
}

func (f *toolFixture) post(t *testing.T, body, key string) *http.Response {
	t.Helper()
	if key == "" {
		key = uuid.New()
	}
	req, _ := http.NewRequest(http.MethodPost, f.srv.URL+"/v1/inferences", strings.NewReader(body))
	req.Header.Set("Authorization", "Bearer owner-key")
	req.Header.Set("Idempotency-Key", key)
	client := http.Client{CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }}
	res, err := client.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { res.Body.Close() })
	return res
}

func (f *toolFixture) json(t *testing.T, body string) map[string]any {
	t.Helper()
	res := f.post(t, body, "")
	var out map[string]any
	if err := json.NewDecoder(res.Body).Decode(&out); err != nil {
		t.Fatal(err)
	}
	if res.StatusCode != 200 {
		t.Fatalf("status %d: %v", res.StatusCode, out)
	}
	return out
}

func readEvents(t *testing.T, res *http.Response) []map[string]any {
	t.Helper()
	var out []map[string]any
	scanner := bufio.NewScanner(res.Body)
	scanner.Buffer(make([]byte, 1<<20), 1<<22)
	for scanner.Scan() {
		if data, ok := strings.CutPrefix(scanner.Text(), "data: "); ok {
			var e map[string]any
			if err := json.Unmarshal([]byte(data), &e); err != nil {
				t.Fatal(err)
			}
			out = append(out, e)
		}
	}
	return out
}

func (f *toolFixture) held(t *testing.T) []string {
	t.Helper()
	rows, err := f.store.Balance(context.Background(), owner)
	if err != nil {
		t.Fatal(err)
	}
	out := []string{}
	for _, r := range rows {
		out = append(out, r.Held+"/"+r.Balance)
	}
	return out
}

func types(events []map[string]any) []string {
	out := []string{}
	for _, e := range events {
		out = append(out, e["type"].(string))
	}
	return out
}

func TestToolCallStreamsInOrderAndSettlesOnce(t *testing.T) {
	f := toolSetup(t, map[string]contracts.Provider{"mock": &router.MockToolProvider{Args: map[string]any{"q": "a"}}})
	list := readEvents(t, f.post(t, streaming(withTools("")), ""))
	want := []string{"request.started", "output.tool_call.delta", "output.tool_call.delta", "output.tool_call.completed", "request.completed"}
	if got := types(list); strings.Join(got, ",") != strings.Join(want, ",") {
		t.Fatalf("events %v", got)
	}
	first, second, done, end := list[1], list[2], list[3], list[4]
	if first["name"] != "lookup" || second["name"] != nil || first["index"] != 0.0 {
		t.Errorf("deltas %v %v", first, second)
	}
	if first["arguments_delta"].(string)+second["arguments_delta"].(string) != `{"q":"a"}` || done["arguments"] != `{"q":"a"}` || done["id"] != first["id"] {
		t.Errorf("completed %v", done)
	}
	usage, _ := json.Marshal(end["usage"])
	if end["finish_reason"] != "tool_calls" || end["billing_status"] != "settled" || string(usage) != `{"meters":{"cache_write":0,"cached":0,"input":10,"output":5},"status":"confirmed"}` {
		t.Errorf("end %v", end)
	}
	for _, row := range f.held(t) {
		if !strings.HasPrefix(row, "0/") {
			t.Errorf("hold left: %v", row)
		}
	}
}

func TestToolCallNonStreamingAndNextTurn(t *testing.T) {
	f := toolSetup(t, map[string]contracts.Provider{"mock": &router.MockToolProvider{Args: map[string]any{"q": "a"}}})
	first := f.json(t, withTools(""))
	calls := first["output"].(map[string]any)["tool_calls"].([]any)
	call := calls[0].(map[string]any)
	if first["finish_reason"] != "tool_calls" || call["name"] != "lookup" || call["arguments"] != `{"q":"a"}` {
		t.Fatalf("first %v", first)
	}
	raw, _ := json.Marshal(call)
	second := f.json(t, `{"service":"chat","customer_id":"c1","input":{"tools":[`+lookup+`],"messages":[{"role":"user","content":"Find it"},`+
		`{"role":"assistant","tool_calls":[`+string(raw)+`]},{"role":"tool","tool_call_id":"`+call["id"].(string)+`","content":"found"}]},"limits":{"max_output_tokens":32}}`)
	out := second["output"].(map[string]any)
	if second["finish_reason"] != "stop" || out["content"] != "The tool finished." || out["tool_calls"] != nil {
		t.Fatalf("second %v", second)
	}
	plain := f.json(t, `{"service":"chat","customer_id":"c1","input":{"messages":[{"role":"user","content":"Hi"}]},"limits":{"max_output_tokens":32}}`)
	if plain["finish_reason"] != "stop" {
		t.Errorf("plain %v", plain)
	}
}

func TestToolIdempotency(t *testing.T) {
	provider := &router.MockToolProvider{}
	f := toolSetup(t, map[string]contracts.Provider{"mock": provider})
	key := uuid.New()
	if res := f.post(t, withTools(""), key); res.StatusCode != 200 {
		t.Fatalf("first %d", res.StatusCode)
	}
	after := f.held(t)
	if res := f.post(t, withTools(""), key); res.StatusCode != http.StatusSeeOther {
		t.Fatalf("retry %d", res.StatusCode)
	}
	changed := strings.Replace(withTools(""), `"name":"lookup"`, `"name":"other"`, 1)
	if res := f.post(t, changed, key); res.StatusCode != http.StatusConflict {
		t.Fatalf("changed tool list %d", res.StatusCode)
	}
	if provider.Calls() != 1 || strings.Join(f.held(t), ",") != strings.Join(after, ",") {
		t.Errorf("ran %d times", provider.Calls())
	}
}

func TestProviderWithoutToolsIsRefusedBeforeReserving(t *testing.T) {
	provider := router.NewMockProvider("")
	f := toolSetup(t, map[string]contracts.Provider{"mock": provider})
	before := f.held(t)
	image := `{"service":"chat","customer_id":"c1","input":{"messages":[{"role":"user","content":[{"type":"image","url":"https://e.example/a.png"}]}]},"limits":{"max_output_tokens":32}}`
	history := `{"service":"chat","customer_id":"c1","input":{"messages":[{"role":"user","content":"Hi"},{"role":"assistant","tool_calls":[{"id":"a","name":"x","arguments":"{}"}]},{"role":"tool","tool_call_id":"a","content":"r"}]},"limits":{"max_output_tokens":32}}`
	for _, body := range []string{withTools(""), image, history} {
		res := f.post(t, body, "")
		raw, _ := io.ReadAll(res.Body)
		if res.StatusCode != 422 || !strings.Contains(string(raw), `"unsupported_capability"`) {
			t.Errorf("%d %s", res.StatusCode, raw)
		}
	}
	if provider.Calls() != 0 || strings.Join(f.held(t), ",") != strings.Join(before, ",") {
		t.Error("something ran or was reserved")
	}
}

func TestRoutingSkipsAProviderThatCannotCarryTools(t *testing.T) {
	text, tools := router.NewMockProvider(""), &router.MockToolProvider{}
	f := toolSetup(t, map[string]contracts.Provider{"text": text, "tools": tools}, offering("a-text", "text"), offering("b-tools", "tools"))
	f.json(t, withTools(""))
	if text.Calls() != 0 || tools.Calls() != 1 {
		t.Fatalf("text %d tools %d", text.Calls(), tools.Calls())
	}
	f.json(t, `{"service":"chat","customer_id":"c1","input":{"messages":[{"role":"user","content":"Hi"}]},"limits":{"max_output_tokens":32}}`)
	if text.Calls() != 1 {
		t.Errorf("plain text did not reach the first offering")
	}
}

func TestTruncatedCallIsNeverComplete(t *testing.T) {
	f := toolSetup(t, map[string]contracts.Provider{"mock": &router.MockToolProvider{Args: map[string]any{"q": "abcdef"}, Behavior: "truncated"}})
	list := readEvents(t, f.post(t, streaming(withTools("")), ""))
	for _, kind := range types(list) {
		if kind == "output.tool_call.completed" {
			t.Fatal("a cut-off call was completed")
		}
	}
	end := list[len(list)-1]
	if end["type"] != "request.failed" || end["execution_status"] != "partial" || end["finish_reason"] != "length" || end["usage"].(map[string]any)["status"] != "confirmed" {
		t.Errorf("end %v", end)
	}
	res := f.post(t, withTools(""), "")
	var plain map[string]any
	_ = json.NewDecoder(res.Body).Decode(&plain)
	if plain["output"].(map[string]any)["tool_calls"] != nil || plain["finish_reason"] != "length" {
		t.Errorf("non-streaming %v", plain)
	}
}

type rogue struct{}

func (rogue) Capabilities() contracts.ProviderCapabilities {
	return contracts.ProviderCapabilities{Tools: true}
}
func (rogue) Invoke(_ context.Context, _ contracts.Invocation, _ contracts.Offering, emit func(contracts.ProviderEvent) error) error {
	if err := emit(contracts.ProviderEvent{Type: "tool_call", ID: "x", Name: "rogue", ArgumentsDelta: "{}"}); err != nil {
		return err
	}
	if err := emit(contracts.ProviderEvent{Type: "usage", Usage: contracts.Meters{Input: 1, Output: 1}}); err != nil {
		return err
	}
	return emit(contracts.ProviderEvent{Type: "done"})
}

func TestUndeclaredToolCallKeepsTheHold(t *testing.T) {
	f := toolSetup(t, map[string]contracts.Provider{"mock": rogue{}})
	list := readEvents(t, f.post(t, `{"service":"chat","customer_id":"c1","input":{"messages":[{"role":"user","content":"Hi"}]},"limits":{"max_output_tokens":32},"stream":true}`, ""))
	end := list[len(list)-1]
	if end["type"] != "request.failed" || end["execution_status"] == "succeeded" {
		t.Fatalf("end %v", end)
	}
	kept := false
	for _, row := range f.held(t) {
		kept = kept || !strings.HasPrefix(row, "0/")
	}
	if !kept {
		t.Error("the hold was released")
	}
}

func TestInferenceBodiesHaveTheirOwnLimit(t *testing.T) {
	f := toolSetup(t, map[string]contracts.Provider{"mock": &router.MockToolProvider{}})
	large := strings.Repeat("x", 250_000)
	accepted := f.post(t, `{"service":"chat","customer_id":"c1","input":{"tools":[`+lookup+`],"messages":[{"role":"user","content":"Go"},`+
		`{"role":"assistant","tool_calls":[{"id":"a","name":"lookup","arguments":"{}"},{"id":"b","name":"lookup","arguments":"{}"}]},`+
		`{"role":"tool","tool_call_id":"a","content":"`+large+`"},{"role":"tool","tool_call_id":"b","content":"`+large+`"}]},"limits":{"max_output_tokens":32}}`, "")
	if accepted.StatusCode != 200 {
		raw, _ := io.ReadAll(accepted.Body)
		t.Fatalf("accepted %d %s", accepted.StatusCode, raw)
	}
	if refused := f.post(t, `{"pad":"`+strings.Repeat("x", 4_000_001)+`"}`, ""); refused.StatusCode != 413 {
		t.Errorf("refused %d", refused.StatusCode)
	}
	req, _ := http.NewRequest(http.MethodPost, f.srv.URL+"/v1/admin/credits/grants", strings.NewReader(`{"pad":"`+strings.Repeat("x", server.MaxBodyBytes)+`"}`))
	req.Header.Set("Authorization", "Bearer owner-key")
	req.Header.Set("Idempotency-Key", "g")
	res, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	res.Body.Close()
	if res.StatusCode != 413 {
		t.Errorf("other routes keep 256 KB: %d", res.StatusCode)
	}
}

func TestCallSignatureTravelsWithTheCallAndComesBackOnTheNextTurn(t *testing.T) {
	const signature = "c2lnLUE9"
	f := toolSetup(t, map[string]contracts.Provider{"mock": &router.MockToolProvider{Args: map[string]any{"q": "a"}, Signature: signature}})
	list := readEvents(t, f.post(t, streaming(withTools("")), ""))
	if list[1]["signature"] != signature || list[3]["signature"] != signature || list[2]["signature"] != nil {
		t.Fatalf("signature in events: %v", list)
	}
	first := f.json(t, withTools(""))
	call := first["output"].(map[string]any)["tool_calls"].([]any)[0].(map[string]any)
	if call["signature"] != signature {
		t.Fatalf("call %v", call)
	}
	callJSON, _ := json.Marshal(call)
	replay := `{"service":"chat","customer_id":"c1","input":{"messages":[{"role":"user","content":"Find it"},` +
		`{"role":"assistant","tool_calls":[` + string(callJSON) + `]},{"role":"tool","tool_call_id":"` + call["id"].(string) + `","content":"found"}],` +
		`"tools":[` + lookup + `]},"limits":{"max_output_tokens":32}}`
	if done := f.json(t, replay); done["finish_reason"] != "stop" {
		t.Fatalf("replay %v", done)
	}
}
