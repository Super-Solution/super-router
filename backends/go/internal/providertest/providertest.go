// SPDX-License-Identifier: Apache-2.0

// Package providertest runs the provider cases of conformance/tools.v1.json against a contracts.Provider: the exact
// upstream request body built from an invocation, and the events produced from a recorded upstream stream.
package providertest

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"reflect"
	"testing"

	"github.com/Super-Solution/super-router/backends/go/contracts"
)

type requestCase struct {
	Name       string          `json:"name"`
	Invocation json.RawMessage `json:"invocation"`
	Body       json.RawMessage `json:"body"`
}

type streamCase struct {
	Name       string            `json:"name"`
	Invocation json.RawMessage   `json:"invocation"`
	Frames     []json.RawMessage `json:"frames"`
	Events     json.RawMessage   `json:"events"`
	Error      string            `json:"error"`
}

// Run checks a provider against the named section ("openai" or "anthropic"). make builds the provider pointed at a
// test server's base URL.
func Run(t *testing.T, section string, make func(baseURL string) contracts.Provider) {
	t.Helper()
	RunFile(t, filepath.Join("..", "..", "..", "..", "conformance", "tools.v1.json"), section, make)
}

// RunFile is Run for a file of the same shape at path. Providers the TypeScript reference does not have (Gemini)
// keep their cases in a file of their own, because tools.v1.json is generated from the reference.
func RunFile(t *testing.T, path, section string, make func(baseURL string) contracts.Provider) {
	t.Helper()
	raw, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	var data map[string]json.RawMessage
	if err := json.Unmarshal(raw, &data); err != nil {
		t.Fatal(err)
	}
	var offering contracts.Offering
	if err := json.Unmarshal(data["offering"], &offering); err != nil {
		t.Fatal(err)
	}
	var cases struct {
		Requests []requestCase `json:"requests"`
		Streams  []streamCase  `json:"streams"`
	}
	if err := json.Unmarshal(data[section], &cases); err != nil {
		t.Fatal(err)
	}
	if len(cases.Requests) < 5 || len(cases.Streams) < 5 {
		t.Fatalf("%s: only %d requests and %d streams", section, len(cases.Requests), len(cases.Streams))
	}
	for _, c := range cases.Requests {
		t.Run("request "+c.Name, func(t *testing.T) {
			var body []byte
			invoke(t, make, offering, c.Invocation, func(w http.ResponseWriter, r *http.Request) {
				body, _ = io.ReadAll(r.Body)
			})
			if have, want := generic(t, body), generic(t, c.Body); !reflect.DeepEqual(have, want) {
				t.Errorf("body\n got  %s\n want %s", body, c.Body)
			}
		})
	}
	for _, c := range cases.Streams {
		t.Run("stream "+c.Name, func(t *testing.T) {
			events, err := invoke(t, make, offering, c.Invocation, func(w http.ResponseWriter, r *http.Request) {
				w.Header().Set("Content-Type", "text/event-stream")
				for _, f := range c.Frames {
					var line bytes.Buffer
					_ = json.Compact(&line, f)
					fmt.Fprintf(w, "data: %s\n\n", line.Bytes())
				}
			})
			var pe *contracts.ProviderError
			switch {
			case c.Error == "" && err != nil:
				t.Errorf("error %v", err)
			case c.Error != "" && (!errors.As(err, &pe) || pe.Code != c.Error):
				t.Errorf("error %v, want %s", err, c.Error)
			}
			have, _ := json.Marshal(events)
			if !reflect.DeepEqual(generic(t, have), generic(t, c.Events)) {
				t.Errorf("events\n got  %s\n want %s", have, c.Events)
			}
		})
	}
}

func invoke(t *testing.T, make func(string) contracts.Provider, offering contracts.Offering, rawInvocation []byte, handler http.HandlerFunc) ([]any, error) {
	t.Helper()
	in, err := contracts.ParseInvocation(rawInvocation)
	if err != nil {
		t.Fatalf("invocation: %v", err)
	}
	srv := httptest.NewServer(handler)
	defer srv.Close()
	events := []any{}
	err = make(srv.URL).Invoke(context.Background(), in, offering, func(e contracts.ProviderEvent) error {
		events = append(events, wire(e))
		return nil
	})
	return events, err
}

// wire is the event as the reference writes it.
func wire(e contracts.ProviderEvent) map[string]any {
	switch e.Type {
	case "delta":
		return map[string]any{"type": "delta", "text": e.Text}
	case "tool_call":
		out := map[string]any{"type": "tool_call", "index": e.Index, "arguments_delta": e.ArgumentsDelta}
		if e.ID != "" {
			out["id"], out["name"] = e.ID, e.Name
		}
		if e.Signature != "" {
			out["signature"] = e.Signature
		}
		return out
	case "usage":
		out := map[string]any{"type": "usage", "usage": e.Usage}
		if e.UpstreamID != "" {
			out["upstream_id"] = e.UpstreamID
		}
		return out
	}
	out := map[string]any{"type": e.Type}
	if e.Completion != "" {
		out["completion"] = e.Completion
	}
	return out
}

func generic(t *testing.T, raw []byte) any {
	t.Helper()
	var v any
	if err := json.Unmarshal(raw, &v); err != nil {
		t.Fatalf("%v: %s", err, raw)
	}
	return v
}
