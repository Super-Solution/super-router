// SPDX-License-Identifier: Apache-2.0

package gemini_test

import (
	"context"
	"errors"
	"fmt"
	"net/http"
	"net/http/httptest"
	"path/filepath"
	"strings"
	"testing"

	"github.com/Super-Solution/super-router/backends/go/contracts"
	"github.com/Super-Solution/super-router/backends/go/internal/providertest"
	"github.com/Super-Solution/super-router/backends/go/provider/gemini"
)

func invocation() contracts.Invocation {
	in := contracts.Invocation{Service: "s", CustomerID: "c", PayerMode: "developer"}
	in.Input.Messages = []contracts.Message{{Role: "user", Content: "hi"}}
	in.Limits.MaxOutputTokens = 16
	return in
}

const final = `data: {"candidates":[{"content":{"role":"model","parts":[{"text":"ok"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":1,"candidatesTokenCount":1,"totalTokenCount":2},"responseId":"r"}` + "\n\n"

func stream(body string) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "text/event-stream")
		fmt.Fprint(w, body)
	}
}

func run(t *testing.T, handler http.HandlerFunc, key, model string, in contracts.Invocation, opts ...gemini.Option) ([]contracts.ProviderEvent, error) {
	t.Helper()
	srv := httptest.NewServer(handler)
	defer srv.Close()
	p := gemini.New(func() string { return key }, append([]gemini.Option{gemini.WithBaseURL(srv.URL)}, opts...)...)
	var events []contracts.ProviderEvent
	err := p.Invoke(context.Background(), in, contracts.Offering{UpstreamModel: model}, func(e contracts.ProviderEvent) error {
		events = append(events, e)
		return nil
	})
	return events, err
}

func code(err error) (definitive, retryable bool, code string) {
	var pe *contracts.ProviderError
	if !errors.As(err, &pe) {
		return false, false, fmt.Sprint(err)
	}
	return pe.Definitive, pe.Retryable, pe.Code
}

func TestConformance(t *testing.T) {
	providertest.RunFile(t, filepath.Join("..", "..", "..", "rust", "conformance", "provider-gemini.v1.json"), "gemini", func(base string) contracts.Provider {
		return gemini.New(func() string { return "test-key" }, gemini.WithBaseURL(base))
	})
}

func TestGeminiAPIGetsTheKeyHeaderAndPath(t *testing.T) {
	var path, key, auth string
	events, err := run(t, func(w http.ResponseWriter, r *http.Request) {
		path, key, auth = r.URL.RequestURI(), r.Header.Get("x-goog-api-key"), r.Header.Get("Authorization")
		stream(final)(w, r)
	}, "g-key", "gemini-test", invocation())
	if err != nil || len(events) == 0 || events[len(events)-1].Completion != "succeeded" {
		t.Fatalf("%v %v", events, err)
	}
	if path != "/v1beta/models/gemini-test:streamGenerateContent?alt=sse" || key != "g-key" || auth != "" {
		t.Errorf("path %q key %q auth %q", path, key, auth)
	}
}

func TestVertexGetsABearerTokenAndItsOwnPath(t *testing.T) {
	var path, key, auth string
	_, err := run(t, func(w http.ResponseWriter, r *http.Request) {
		path, key, auth = r.URL.RequestURI(), r.Header.Get("x-goog-api-key"), r.Header.Get("Authorization")
		stream(final)(w, r)
	}, "ya29.token", "models/gemini-test", invocation(),
		gemini.WithPathPrefix("/v1/projects/p/locations/us-central1/publishers/google/"), gemini.WithBearerAuth())
	if err != nil {
		t.Fatal(err)
	}
	if want := "/v1/projects/p/locations/us-central1/publishers/google/models/gemini-test:streamGenerateContent?alt=sse"; path != want {
		t.Errorf("path %q", path)
	}
	if auth != "Bearer ya29.token" || key != "" {
		t.Errorf("auth %q key %q", auth, key)
	}
}

func TestModelNamesThatCouldChangeThePathAreRefusedBeforeAnythingIsSent(t *testing.T) {
	called := false
	for _, model := range []string{"", "a/b", "a?x=1", "a#b", "../x", "a b", "models/", "geméni"} {
		events, err := run(t, func(w http.ResponseWriter, r *http.Request) { called = true }, "k", model, invocation())
		if d, r, c := code(err); len(events) != 0 || !d || r || c != "request_invalid" {
			t.Errorf("%q: %v %v %v %v", model, events, d, r, c)
		}
	}
	if called {
		t.Error("something was dispatched")
	}
}

func TestRequestsTheUpstreamWouldRejectNeverLeaveTheProcess(t *testing.T) {
	call := func(arguments string) contracts.Message {
		return contracts.Message{Role: "assistant", ToolCalls: []contracts.ToolCall{{ID: "c1", Name: "lookup", Arguments: arguments}}}
	}
	result := func(id string) contracts.Message {
		return contracts.Message{Role: "tool", ToolCallID: id, Content: "x"}
	}
	user := contracts.Message{Role: "user", Content: "hi"}
	cases := []struct {
		name     string
		messages []contracts.Message
		code     string
	}{
		{"arguments that are not an object", []contracts.Message{user, call("[1]"), result("c1")}, "request_invalid"},
		{"arguments that are not JSON", []contracts.Message{user, call("{"), result("c1")}, "request_invalid"},
		{"arguments with trailing text", []contracts.Message{user, call(`{} x`), result("c1")}, "request_invalid"},
		{"a result for a call never made", []contracts.Message{user, result("nope")}, "request_invalid"},
		{"an image that is a link", []contracts.Message{{Role: "user", Parts: []contracts.ContentPart{{Type: "image", URL: "https://e.example/a.png"}}}}, "request_invalid"},
		{"nothing but a system message", []contracts.Message{{Role: "system", Content: "Be brief"}}, "messages_required"},
	}
	called := false
	for _, c := range cases {
		in := invocation()
		in.Input.Messages = c.messages
		events, err := run(t, func(w http.ResponseWriter, r *http.Request) { called = true }, "k", "gemini-test", in)
		if d, r, got := code(err); len(events) != 0 || !d || r || got != c.code {
			t.Errorf("%s: %v %v %v %v", c.name, events, d, r, got)
		}
	}
	if called {
		t.Error("something was dispatched")
	}
	// No arguments at all is an empty object, not a refusal.
	in := invocation()
	in.Input.Messages = []contracts.Message{user, call(""), result("c1")}
	if _, err := run(t, stream(final), "k", "gemini-test", in); err != nil {
		t.Errorf("empty arguments: %v", err)
	}
}

func TestFailuresAreClassified(t *testing.T) {
	status := func(code int) http.HandlerFunc {
		return func(w http.ResponseWriter, r *http.Request) { w.WriteHeader(code) }
	}
	refusal := func(code int, message string) http.HandlerFunc {
		return func(w http.ResponseWriter, r *http.Request) {
			w.WriteHeader(code)
			fmt.Fprintf(w, `{"error":{"code":%d,"message":%q,"status":"INVALID_ARGUMENT"}}`, code, message)
		}
	}
	task := invocation()
	task.Input.Task = &contracts.Task{}
	cases := []struct {
		name    string
		handler http.HandlerFunc
		key     string
		in      contracts.Invocation
		want    [3]any
	}{
		{"rate limited", status(429), "k", invocation(), [3]any{true, true, "rate_limited"}},
		{"bad request", status(400), "k", invocation(), [3]any{true, false, "upstream_http_400"}},
		{"unauthorized", status(401), "k", invocation(), [3]any{true, true, "credentials_rejected"}},
		{"forbidden", status(403), "k", invocation(), [3]any{true, true, "credentials_rejected"}},
		{"a signature from another backend", refusal(400, "Corrupted thought signature."), "k", invocation(), [3]any{true, true, "thought_signature_rejected"}},
		{"the other backend's wording", refusal(400, "Invalid thought_signature in the request"), "k", invocation(), [3]any{true, true, "thought_signature_rejected"}},
		{"another bad request with a message", refusal(400, "Unknown name \"x\""), "k", invocation(), [3]any{true, false, "upstream_http_400"}},
		{"a signature message on another status", refusal(500, "Corrupted thought signature."), "k", invocation(), [3]any{false, false, "upstream_http_500"}},
		{"not found", status(404), "k", invocation(), [3]any{true, true, "model_unavailable"}},
		{"a key the API says is not valid", refusal(400, "API key not valid. Please pass a valid API key."), "k", invocation(), [3]any{true, true, "credentials_rejected"}},
		{"a region the API refuses", refusal(400, "User location is not supported for the API use."), "k", invocation(), [3]any{true, true, "region_blocked"}},
		{"an empty balance reported as 429", refusal(429, "You exceeded your current quota, please check your plan and billing details."), "k", invocation(), [3]any{true, true, "quota_exhausted"}},
		{"a per-minute 429 stays a rate limit", refusal(429, "Quota exceeded for metric requests per minute"), "k", invocation(), [3]any{true, true, "rate_limited"}},
		{"another client error is the request's", refusal(422, "bad"), "k", invocation(), [3]any{true, false, "upstream_http_422"}},
		{"a server error with a key message keeps the hold", refusal(503, "API key not valid"), "k", invocation(), [3]any{false, false, "upstream_http_503"}},
		{"server error is unknown", status(500), "k", invocation(), [3]any{false, false, "upstream_http_500"}},
		{"unavailable is unknown", status(503), "k", invocation(), [3]any{false, false, "upstream_http_503"}},
		{"empty 200 is interrupted", status(200), "k", invocation(), [3]any{false, false, "stream_interrupted"}},
		{"no credentials", status(200), "", invocation(), [3]any{true, false, "credentials_missing"}},
		{"a key that cannot be a header", status(200), "bad\nkey", invocation(), [3]any{true, false, "request_invalid"}},
		{"tasks unsupported", status(200), "k", task, [3]any{true, false, "unsupported_task"}},
	}
	for _, c := range cases {
		events, err := run(t, c.handler, c.key, "gemini-test", c.in)
		d, r, got := code(err)
		if len(events) != 0 || d != c.want[0] || r != c.want[1] || got != c.want[2] {
			t.Errorf("%s: %v %v %v %v", c.name, events, d, r, got)
		}
	}
}

func TestARedirectIsRefusedAndNeverFollowed(t *testing.T) {
	hit := false
	target := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) { hit = true }))
	defer target.Close()
	_, err := run(t, func(w http.ResponseWriter, r *http.Request) {
		http.Redirect(w, r, target.URL+"/elsewhere", http.StatusTemporaryRedirect)
	}, "k", "gemini-test", invocation())
	if d, r, c := code(err); d || r || c != "upstream_http_307" {
		t.Errorf("%v %v %v", d, r, c)
	}
	if hit {
		t.Error("the redirect target was contacted")
	}
}

func TestNormalize(t *testing.T) {
	n := func(v int64) *int64 { return &v }
	cases := []struct {
		name string
		u    gemini.UsageMetadata
		want contracts.Meters
		ok   bool
	}{
		{"cached out of the prompt, thoughts into output", gemini.UsageMetadata{PromptTokenCount: n(1000), CachedContentTokenCount: n(400), CandidatesTokenCount: n(50), ThoughtsTokenCount: n(30), TotalTokenCount: n(1080)}, contracts.Meters{Input: 600, Cached: 400, Output: 80}, true},
		{"tool-use prompt tokens are input", gemini.UsageMetadata{PromptTokenCount: n(100), ToolUsePromptTokenCount: n(25), CandidatesTokenCount: n(10), TotalTokenCount: n(135)}, contracts.Meters{Input: 125, Output: 10}, true},
		{"a larger total adds the rest to output", gemini.UsageMetadata{PromptTokenCount: n(100), CandidatesTokenCount: n(10), TotalTokenCount: n(150)}, contracts.Meters{Input: 100, Output: 50}, true},
		{"a smaller total changes nothing", gemini.UsageMetadata{PromptTokenCount: n(100), CandidatesTokenCount: n(10), TotalTokenCount: n(50)}, contracts.Meters{Input: 100, Output: 10}, true},
		{"thoughts folded into the candidates are not added twice", gemini.UsageMetadata{PromptTokenCount: n(100), CandidatesTokenCount: n(40), ThoughtsTokenCount: n(30), TotalTokenCount: n(140)}, contracts.Meters{Input: 100, Output: 40}, true},
		{"thoughts counted apart are added", gemini.UsageMetadata{PromptTokenCount: n(100), CandidatesTokenCount: n(40), ThoughtsTokenCount: n(30), TotalTokenCount: n(170)}, contracts.Meters{Input: 100, Output: 70}, true},
		{"folded with cache and tool use", gemini.UsageMetadata{PromptTokenCount: n(100), CachedContentTokenCount: n(20), ToolUsePromptTokenCount: n(5), CandidatesTokenCount: n(40), ThoughtsTokenCount: n(30), TotalTokenCount: n(145)}, contracts.Meters{Input: 85, Cached: 20, Output: 40}, true},
		{"no total, thoughts apart", gemini.UsageMetadata{PromptTokenCount: n(100), CandidatesTokenCount: n(40), ThoughtsTokenCount: n(30)}, contracts.Meters{Input: 100, Output: 70}, true},
		{"only the prompt count is required", gemini.UsageMetadata{PromptTokenCount: n(7), TotalTokenCount: n(7)}, contracts.Meters{Input: 7}, true},
		{"no prompt count", gemini.UsageMetadata{}, contracts.Meters{}, false},
		{"more cached than prompt", gemini.UsageMetadata{PromptTokenCount: n(10), CachedContentTokenCount: n(11)}, contracts.Meters{}, false},
		{"negative", gemini.UsageMetadata{PromptTokenCount: n(-1)}, contracts.Meters{}, false},
		{"overflow in input", gemini.UsageMetadata{PromptTokenCount: n(1<<63 - 1), ToolUsePromptTokenCount: n(1)}, contracts.Meters{}, false},
		{"overflow in output", gemini.UsageMetadata{PromptTokenCount: n(1), CandidatesTokenCount: n(1<<63 - 1), ThoughtsTokenCount: n(1)}, contracts.Meters{}, false},
	}
	for _, c := range cases {
		got, ok := gemini.Normalize(c.u)
		if ok != c.ok || (ok && (got.Input != c.want.Input || got.Cached != c.want.Cached || got.CacheWrite != c.want.CacheWrite || got.Output != c.want.Output)) {
			t.Errorf("%s: %+v %v", c.name, got, ok)
		}
	}
}

func TestTheKeyIsNeverInAnError(t *testing.T) {
	_, err := run(t, func(w http.ResponseWriter, r *http.Request) { w.WriteHeader(401) }, "super-secret", "gemini-test", invocation())
	if err == nil || strings.Contains(err.Error(), "super-secret") {
		t.Errorf("%v", err)
	}
}
