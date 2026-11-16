// SPDX-License-Identifier: Apache-2.0

package anthropic_test

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/Super-Solution/super-router/backends/go/contracts"
	"github.com/Super-Solution/super-router/backends/go/provider/anthropic"
)

func invocation() contracts.Invocation {
	in := contracts.Invocation{Service: "s", CustomerID: "c", PayerMode: "developer"}
	in.Input.Messages = []contracts.Message{{Role: "system", Content: "be brief"}, {Role: "user", Content: "hi"}}
	in.Limits.MaxOutputTokens = 16
	return in
}

func frame(data string) string { return "event: x\ndata: " + data + "\n\n" }

func stream(frames ...string) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "text/event-stream")
		for _, f := range frames {
			fmt.Fprint(w, f)
		}
	}
}

func run(t *testing.T, handler http.HandlerFunc, key string, in contracts.Invocation) ([]contracts.ProviderEvent, error) {
	t.Helper()
	srv := httptest.NewServer(handler)
	defer srv.Close()
	p := anthropic.New(func() string { return key }, anthropic.WithBaseURL(srv.URL))
	var events []contracts.ProviderEvent
	err := p.Invoke(context.Background(), in, contracts.Offering{UpstreamModel: "claude-test"}, func(e contracts.ProviderEvent) error {
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

func TestStreamsTextAndMergesUsage(t *testing.T) {
	var headers http.Header
	var body string
	events, err := run(t, func(w http.ResponseWriter, r *http.Request) {
		headers = r.Header.Clone()
		b, _ := io.ReadAll(r.Body)
		body = string(b)
		stream(
			frame(`{"type":"message_start","message":{"id":"msg_1","usage":{"input_tokens":50,"cache_read_input_tokens":20,"cache_creation_input_tokens":5,"output_tokens":1,"cache_creation":{"ephemeral_5m_input_tokens":5}}}}`),
			frame(`{"type":"content_block_delta","delta":{"type":"text_delta","text":"Hi"}}`),
			frame(`{"type":"content_block_delta","delta":{"type":"input_json_delta","partial_json":"{}"}}`),
			frame(`{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":9,"server_tool_use":{"web_search_requests":0}}}`),
			frame(`{"type":"message_stop"}`),
		)(w, r)
	}, "key", invocation())
	if err != nil {
		t.Fatal(err)
	}
	if headers.Get("x-api-key") != "key" || headers.Get("anthropic-version") != "2023-06-01" ||
		!strings.Contains(body, `"system":"be brief"`) || strings.Contains(body, `"role":"system"`) || !strings.Contains(body, `"max_tokens":16`) {
		t.Fatalf("request %v %s", headers, body)
	}
	if len(events) != 3 || events[0].Text != "Hi" {
		t.Fatalf("%+v", events)
	}
	if events[1].Usage != (contracts.Meters{Input: 50, Cached: 20, CacheWrite: 5, Output: 9}) || events[1].UpstreamID != "msg_1" || events[2].Completion != "succeeded" {
		t.Fatalf("%+v", events)
	}
}

func TestMaxTokensIsPartial(t *testing.T) {
	events, err := run(t, stream(
		frame(`{"type":"message_start","message":{"id":"m","usage":{"input_tokens":1,"output_tokens":1}}}`),
		frame(`{"type":"message_delta","delta":{"stop_reason":"max_tokens"},"usage":{"output_tokens":16}}`),
		frame(`{"type":"message_stop"}`),
	), "k", invocation())
	if err != nil || events[len(events)-1].Completion != "partial" {
		t.Fatalf("%v %+v", err, events)
	}
}

func TestFailures(t *testing.T) {
	status := func(n int) http.HandlerFunc {
		return func(w http.ResponseWriter, r *http.Request) { w.WriteHeader(n) }
	}
	task := invocation()
	task.Input.Task = &contracts.Task{Kind: "image", Prompt: "x"}
	onlySystem := invocation()
	onlySystem.Input.Messages = onlySystem.Input.Messages[:1]
	start := frame(`{"type":"message_start","message":{"id":"m","usage":{"input_tokens":1,"output_tokens":1}}}`)
	cases := []struct {
		name              string
		handler           http.HandlerFunc
		key               string
		in                contracts.Invocation
		definitive, retry bool
		code              string
	}{
		{"rate limited", status(429), "k", invocation(), true, true, "upstream_http_429"},
		{"bad request", status(400), "k", invocation(), true, false, "upstream_http_400"},
		{"overloaded is unknown", status(529), "k", invocation(), false, false, "upstream_http_529"},
		{"no credentials", status(200), "", invocation(), true, false, "credentials_missing"},
		{"tasks unsupported", status(200), "k", task, true, false, "unsupported_task"},
		{"only system messages", status(200), "k", onlySystem, true, false, "messages_required"},
		{"truncated stream", stream(start), "k", invocation(), false, false, "stream_interrupted"},
		{"stop without start", stream(frame(`{"type":"message_stop"}`)), "k", invocation(), false, false, "usage_unknown"},
		{"start without counters is not free", stream(frame(`{"type":"message_start","message":{"id":"m","usage":{}}}`), frame(`{"type":"message_stop"}`)), "k", invocation(), false, false, "invalid_usage"},
		{"start without output", stream(frame(`{"type":"message_start","message":{"id":"m","usage":{"input_tokens":3}}}`), frame(`{"type":"message_stop"}`)), "k", invocation(), false, false, "invalid_usage"},
		{"non-integer counter", stream(start, frame(`{"type":"message_delta","delta":{},"usage":{"output_tokens":"9"}}`)), "k", invocation(), false, false, "invalid_usage"},
		// json.Unmarshal accepts null and leaves the zero value, which would overwrite the real count from message_start
		// and bill the call as free.
		{"null input counter in message_delta", stream(start, frame(`{"type":"message_delta","delta":{},"usage":{"input_tokens":null}}`)), "k", invocation(), false, false, "invalid_usage"},
		{"null output counter in message_delta", stream(start, frame(`{"type":"message_delta","delta":{},"usage":{"output_tokens":null}}`)), "k", invocation(), false, false, "invalid_usage"},
		{"null cache read counter in message_delta", stream(start, frame(`{"type":"message_delta","delta":{},"usage":{"cache_read_input_tokens":null}}`)), "k", invocation(), false, false, "invalid_usage"},
		{"null cache write counter in message_delta", stream(start, frame(`{"type":"message_delta","delta":{},"usage":{"cache_creation_input_tokens":null}}`)), "k", invocation(), false, false, "invalid_usage"},
		{"error event", stream(start, frame(`{"type":"error"}`)), "k", invocation(), false, false, "upstream_stream_error"},
		{"bad event json", stream(frame(`{`)), "k", invocation(), false, false, "invalid_upstream_event"},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			_, err := run(t, c.handler, c.key, c.in)
			d, r, got := code(err)
			if d != c.definitive || r != c.retry || got != c.code {
				t.Fatalf("definitive=%v retryable=%v code=%s, want %v %v %s", d, r, got, c.definitive, c.retry, c.code)
			}
		})
	}
}

func TestDeltaMayCompleteTheCounters(t *testing.T) {
	events, err := run(t, stream(
		frame(`{"type":"message_start","message":{"id":"m","usage":{"input_tokens":7}}}`),
		frame(`{"type":"message_delta","delta":{},"usage":{"output_tokens":4}}`),
		frame(`{"type":"message_stop"}`),
	), "k", invocation())
	if err != nil || events[0].Usage != (contracts.Meters{Input: 7, Output: 4}) {
		t.Fatalf("%v %+v", err, events)
	}
}

// A null counter in message_delta must never replace a real count from message_start: that would settle the call
// as if no tokens were used.
func TestNullCounterCannotZeroARealCount(t *testing.T) {
	start := frame(`{"type":"message_start","message":{"id":"m","usage":{"input_tokens":7,"output_tokens":3}}}`)
	null := frame(`{"type":"message_delta","delta":{},"usage":{"output_tokens":null}}`)
	events, err := run(t, stream(start, null, frame(`{"type":"message_stop"}`)), "k", invocation())
	if definitive, retryable, got := code(err); definitive || retryable || got != "invalid_usage" {
		t.Fatalf("definitive=%v retryable=%v code=%s, want an uncertain invalid_usage", definitive, retryable, got)
	}
	for _, e := range events {
		if e.Type == "usage" || e.Type == "done" {
			t.Fatalf("the response was accepted and billed with %+v", e)
		}
	}
}
