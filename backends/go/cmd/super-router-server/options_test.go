// SPDX-License-Identifier: Apache-2.0

package main

import (
	"context"
	"fmt"
	"io"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync/atomic"
	"testing"
)

func envOf(pairs map[string]string) func(string) string {
	return func(k string) string { return pairs[k] }
}

func TestProviderAndHealthOptionsAreCheckedAndNeverEchoed(t *testing.T) {
	for _, c := range [][2]string{
		{"OPENAI_BASE_URL", "ftp://host"},
		{"OPENAI_BASE_URL", "llm.internal"},
		{"OPENAI_BASE_URL", "https://user:secret-pass@llm.internal"},
		{"OPENAI_BASE_URL", "https://llm.internal/v1"},
		{"OPENAI_BASE_URL", "https://llm.internal/openai/v1/"},
		{"ANTHROPIC_BASE_URL", "https://llm.internal/?key=secret-pass"},
		{"GEMINI_BASE_URL", "https://generativelanguage.googleapis.com/v1beta"},
		{"GEMINI_BASE_URL", "https://"},
		{"SUPERROUTER_HEALTH", "yes"},
		{"SUPERROUTER_HEALTH_MAX_MS", "1000"},
	} {
		env := map[string]string{"SUPERROUTER_API_KEY": apiKey, "SUPERROUTER_DIGEST_KEY": strings.Repeat("d", 32), "ADDR": "127.0.0.1:0", c[0]: c[1]}
		err := run(context.Background(), envOf(env), slog.New(slog.NewTextHandler(io.Discard, nil)), nil)
		if err == nil {
			t.Fatalf("accepted %s=%s", c[0], c[1])
		}
		if strings.Contains(err.Error(), "secret-pass") {
			t.Fatalf("%s: the value was echoed: %v", c[0], err)
		}
	}
	for _, bad := range []string{"0", "-1", "abc", "86400001", "1.5"} {
		if _, err := healthPolicy(envOf(map[string]string{"SUPERROUTER_HEALTH": "on", "SUPERROUTER_HEALTH_MAX_MS": bad})); err == nil {
			t.Fatalf("accepted a cooldown of %q", bad)
		}
	}
	if b, err := baseURL("OPENAI_BASE_URL", "https://llm.internal/openai/"); err != nil || b != "https://llm.internal/openai" {
		t.Fatalf("%q %v", b, err)
	}
	if b, err := baseURL("ANTHROPIC_BASE_URL", "http://127.0.0.1:9000"); err != nil || b != "http://127.0.0.1:9000" {
		t.Fatalf("%q %v", b, err)
	}
	policy, err := healthPolicy(envOf(map[string]string{"SUPERROUTER_HEALTH": "on", "SUPERROUTER_HEALTH_RATE_LIMITED_MS": "250"}))
	if err != nil || policy == nil || policy.RateLimitedMs != 250 || policy.CredentialsRejectedMs != 300_000 {
		t.Fatalf("named fields change, the rest keep their defaults: %+v %v", policy, err)
	}
	for _, off := range []map[string]string{{"SUPERROUTER_HEALTH": "off"}, {}} {
		if p, err := healthPolicy(envOf(off)); p != nil || err != nil {
			t.Fatalf("off by default: %+v %v", p, err)
		}
	}
}

// fakeHost answers every POST with status (an empty body) or, for 200, a short Anthropic stream, and counts requests.
func fakeHost(t *testing.T, status int, calls *atomic.Int64) string {
	t.Helper()
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		calls.Add(1)
		if status != 200 {
			w.WriteHeader(status)
			return
		}
		w.Header().Set("Content-Type", "text/event-stream")
		for _, f := range []string{
			`{"type":"message_start","message":{"id":"m","usage":{"input_tokens":3,"output_tokens":1}}}`,
			`{"type":"content_block_delta","delta":{"type":"text_delta","text":"from the second offering"}}`,
			`{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":4}}`,
			`{"type":"message_stop"}`,
		} {
			fmt.Fprintf(w, "event: x\ndata: %s\n\n", f)
		}
	}))
	t.Cleanup(srv.Close)
	return srv.URL
}

// The shipped server end to end with real providers: the first offering's key is rejected by its host (401), the request
// falls through to the second offering, and with SUPERROUTER_HEALTH=on the next request does not touch the rejected
// offering at all, while without it every request pays the failed attempt.
func TestARejectedKeyIsSkippedOnlyWhenTheHealthBookIsOn(t *testing.T) {
	for _, health := range []bool{true, false} {
		t.Run(fmt.Sprintf("health=%v", health), func(t *testing.T) {
			var rejected, serving atomic.Int64
			env := map[string]string{
				"SUPERROUTER_API_KEY": apiKey, "SUPERROUTER_DIGEST_KEY": strings.Repeat("d", 32), "ADDR": "127.0.0.1:0",
				"OPENAI_API_KEY": "sk-wrong", "OPENAI_BASE_URL": fakeHost(t, 401, &rejected),
				"ANTHROPIC_API_KEY": "sk-right", "ANTHROPIC_BASE_URL": fakeHost(t, 200, &serving),
			}
			if health {
				env["SUPERROUTER_HEALTH"] = "on"
			}
			ctx, stop := context.WithCancel(context.Background())
			ready, done := make(chan string, 1), make(chan error, 1)
			go func() { done <- run(ctx, envOf(env), slog.New(slog.NewTextHandler(io.Discard, nil)), ready) }()
			base := "http://" + <-ready
			do := func(method, path, key, body string) (int, string) {
				req, _ := http.NewRequest(method, base+path, strings.NewReader(body))
				req.Header.Set("Authorization", "Bearer "+apiKey)
				req.Header.Set("Idempotency-Key", key)
				req.Header.Set("Content-Type", "application/json")
				res, err := http.DefaultClient.Do(req)
				if err != nil {
					t.Fatal(err)
				}
				defer res.Body.Close()
				raw, _ := io.ReadAll(res.Body)
				return res.StatusCode, string(raw)
			}
			offering := func(id, provider string) string {
				return fmt.Sprintf(`{"id":%q,"provider":%q,"model":"m","upstream_model":"m","region":"test","labels":[],"enabled":true,"source":"byok","max_input_tokens":1024,"max_output_tokens":128,"price":{"asset":"USD","input":"1","cached":"1","cache_write":"1","output":"1","per":1}}`, id, provider)
			}
			service := `{"id":"chat","version":1,"mode":"tier","tier":"chat/low","regions":["test"],"required_labels":[],"price":{"asset":"credit:demo","input":"2","cached":"1","cache_write":"2","output":"3","per":1},"max_attempts":2,"max_provider_cost":"10000","max_output_tokens":128,"weights":{"cost":1,"latency":0,"reliability":0,"quality":0},"cost_scale":"10000","latency_scale_ms":1000,"offerings":[` + offering("a", "openai") + `,` + offering("b", "anthropic") + `]}`
			if status, body := do("POST", "/v1/admin/service-versions", "pub", service); status != 201 {
				t.Fatalf("publish %d %s", status, body)
			}
			if status, body := do("POST", "/v1/admin/credits/grants", "grant", `{"asset":"credit:demo","amount":"100000"}`); status != 201 {
				t.Fatalf("grant %d %s", status, body)
			}
			for n := 1; n <= 3; n++ {
				status, body := do("POST", "/v1/inferences", fmt.Sprintf("infer-%d", n), `{"service":"chat","customer_id":"default","input":{"messages":[{"role":"user","content":"Hello"}]},"limits":{"max_output_tokens":32}}`)
				if status != 200 || !strings.Contains(body, `"execution_status":"succeeded"`) || !strings.Contains(body, "from the second offering") {
					t.Fatalf("request %d: %d %s", n, status, body)
				}
			}
			if serving.Load() != 3 {
				t.Fatalf("the healthy offering served %d requests, want 3", serving.Load())
			}
			// On: one failed attempt, then the rejected offering is left alone (its cooldown is minutes). Off: three.
			want := int64(3)
			if health {
				want = 1
			}
			if rejected.Load() != want {
				t.Fatalf("the rejected offering was called %d times, want %d", rejected.Load(), want)
			}
			stop()
			if err := <-done; err != nil {
				t.Fatal(err)
			}
		})
	}
}
