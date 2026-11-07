// SPDX-License-Identifier: Apache-2.0

package openai_test

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
	"github.com/Super-Solution/super-router/backends/go/provider/openai"
)

func invocation() contracts.Invocation {
	in := contracts.Invocation{Service: "s", CustomerID: "c", PayerMode: "developer"}
	in.Input.Messages = []contracts.Message{{Role: "user", Content: "hi"}}
	in.Limits.MaxOutputTokens = 16
	return in
}

func run(t *testing.T, handler http.HandlerFunc, key string, in contracts.Invocation) ([]contracts.ProviderEvent, error) {
	t.Helper()
	srv := httptest.NewServer(handler)
	defer srv.Close()
	p := openai.New(func() string { return key }, openai.WithBaseURL(srv.URL))
	var events []contracts.ProviderEvent
	err := p.Invoke(context.Background(), in, contracts.Offering{UpstreamModel: "gpt-test"}, func(e contracts.ProviderEvent) error {
		events = append(events, e)
		return nil
	})
	return events, err
}

func stream(frames ...string) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "text/event-stream")
		for _, f := range frames {
			fmt.Fprint(w, f)
		}
	}
}

func frame(data string) string { return "event: x\ndata: " + data + "\n\n" }

func code(err error) (definitive, retryable bool, code string) {
	var pe *contracts.ProviderError
	if !errors.As(err, &pe) {
		return false, false, fmt.Sprint(err)
	}
	return pe.Definitive, pe.Retryable, pe.Code
}

func TestStreamsTextAndNormalisesUsage(t *testing.T) {
	var got struct{ auth, body string }
	events, err := run(t, func(w http.ResponseWriter, r *http.Request) {
		b, _ := io.ReadAll(r.Body)
		got.auth, got.body = r.Header.Get("Authorization"), string(b)
		stream(
			frame(`{"type":"response.output_text.delta","delta":"Hel"}`),
			frame(`{"type":"response.output_text.delta","delta":"lo"}`),
			frame(`{"type":"response.completed","response":{"id":"resp_1","usage":{"input_tokens":100,"output_tokens":7,"input_tokens_details":{"cached_tokens":30,"cache_write_tokens":10}}}}`),
		)(w, r)
	}, "sk-test", invocation())
	if err != nil {
		t.Fatal(err)
	}
	if got.auth != "Bearer sk-test" || !strings.Contains(got.body, `"model":"gpt-test"`) || !strings.Contains(got.body, `"store":false`) || !strings.Contains(got.body, `"max_output_tokens":16`) {
		t.Fatalf("request %q %q", got.auth, got.body)
	}
	if len(events) != 4 || events[0].Text != "Hel" || events[1].Text != "lo" {
		t.Fatalf("%+v", events)
	}
	u := events[2]
	if u.Type != "usage" || u.UpstreamID != "resp_1" || u.Usage != (contracts.Meters{Input: 60, Cached: 30, CacheWrite: 10, Output: 7}) {
		t.Fatalf("usage %+v", u)
	}
	if events[3].Type != "done" || events[3].Completion != "succeeded" {
		t.Fatalf("%+v", events[3])
	}
}

func TestIncompleteIsPartial(t *testing.T) {
	events, err := run(t, stream(frame(`{"type":"response.incomplete","response":{"id":"r","usage":{"input_tokens":1,"output_tokens":2}}}`)), "k", invocation())
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
	cases := []struct {
		name              string
		handler           http.HandlerFunc
		key               string
		in                contracts.Invocation
		definitive, retry bool
		code              string
	}{
		{"rate limited", status(429), "k", invocation(), true, true, "upstream_http_429"},
		{"unauthorized", status(401), "k", invocation(), true, false, "upstream_http_401"},
		{"server error is unknown", status(500), "k", invocation(), false, false, "upstream_http_500"},
		{"redirect is refused", func(w http.ResponseWriter, r *http.Request) { http.Redirect(w, r, "http://127.0.0.1:1/", 302) }, "k", invocation(), false, false, "upstream_http_302"},
		{"no credentials", status(200), "", invocation(), true, false, "credentials_missing"},
		{"tasks unsupported", status(200), "k", task, true, false, "unsupported_task"},
		{"truncated stream", stream(frame(`{"type":"response.output_text.delta","delta":"a"}`)), "k", invocation(), false, false, "stream_interrupted"},
		{"unterminated frame is dropped", stream(`data: {"type":"response.completed"}`), "k", invocation(), false, false, "stream_interrupted"},
		{"bad event json", stream(frame(`not json`)), "k", invocation(), false, false, "invalid_upstream_event"},
		{"completed without usage", stream(frame(`{"type":"response.completed","response":{"id":"r"}}`)), "k", invocation(), false, false, "usage_unknown"},
		{"negative input after cache", stream(frame(`{"type":"response.completed","response":{"id":"r","usage":{"input_tokens":1,"output_tokens":1,"input_tokens_details":{"cached_tokens":5}}}}`)), "k", invocation(), false, false, "invalid_usage"},
		{"upstream error event", stream(frame(`{"type":"error"}`)), "k", invocation(), false, false, "upstream_incomplete"},
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

func TestStopsWhenTheCallerStops(t *testing.T) {
	boom := errors.New("client went away")
	srv := httptest.NewServer(stream(frame(`{"type":"response.output_text.delta","delta":"a"}`), frame(`{"type":"response.output_text.delta","delta":"b"}`)))
	defer srv.Close()
	p := openai.New(func() string { return "k" }, openai.WithBaseURL(srv.URL))
	n := 0
	err := p.Invoke(context.Background(), invocation(), contracts.Offering{}, func(contracts.ProviderEvent) error { n++; return boom })
	if err == nil || n != 1 {
		t.Fatalf("emitted %d events, err %v", n, err)
	}
	if d, _, _ := code(err); d {
		t.Fatal("a stop after output must not be definitive")
	}
}

func TestCancelledContext(t *testing.T) {
	srv := httptest.NewServer(func() http.HandlerFunc {
		return func(w http.ResponseWriter, r *http.Request) { <-r.Context().Done() }
	}())
	defer srv.Close()
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	p := openai.New(func() string { return "k" }, openai.WithBaseURL(srv.URL))
	err := p.Invoke(ctx, invocation(), contracts.Offering{}, func(contracts.ProviderEvent) error { return nil })
	if d, _, c := code(err); d || c != "transport_unknown" {
		t.Fatalf("definitive=%v code=%s", d, c)
	}
}
