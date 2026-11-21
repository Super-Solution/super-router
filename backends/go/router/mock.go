// SPDX-License-Identifier: Apache-2.0

package router

import (
	"context"
	"encoding/json"
	"strings"
	"sync/atomic"
	"time"

	"github.com/Super-Solution/super-router/backends/go/contracts"
	"github.com/Super-Solution/super-router/backends/go/internal/uuid"
)

// MockProvider is a deterministic provider for tests and local runs. Behavior is "success", "reject"
// (a definitive, retryable rejection), "unknown" (outcome lost before any output) or "partial" (output, then loss).
type MockProvider struct {
	Behavior string
	Delay    time.Duration
	calls    atomic.Int64
}

// NewMockProvider creates a MockProvider; an empty behavior means success.
func NewMockProvider(behavior string) *MockProvider { return &MockProvider{Behavior: behavior} }

// Calls is how many times Invoke was called.
func (m *MockProvider) Calls() int64 { return m.calls.Load() }

// Invoke implements contracts.Provider.
func (m *MockProvider) Invoke(ctx context.Context, in contracts.Invocation, _ contracts.Offering, emit func(contracts.ProviderEvent) error) error {
	m.calls.Add(1)
	if in.Input.Task != nil {
		return &contracts.ProviderError{Definitive: true, Code: "unsupported_task"}
	}
	if m.Delay > 0 {
		select {
		case <-time.After(m.Delay):
		case <-ctx.Done():
			return &contracts.ProviderError{Code: "upstream_failed"}
		}
	}
	if m.Behavior == "reject" {
		return &contracts.ProviderError{Definitive: true, Retryable: true, Code: "rate_limited"}
	}
	if m.Behavior == "unknown" || ctx.Err() != nil {
		return &contracts.ProviderError{Code: "upstream_failed"}
	}
	if err := emit(contracts.ProviderEvent{Type: "delta", Text: "Hello from SuperRouter."}); err != nil {
		return err
	}
	if m.Behavior == "partial" {
		return &contracts.ProviderError{Code: "upstream_failed"}
	}
	if err := emit(contracts.ProviderEvent{Type: "usage", Usage: contracts.Meters{Input: 10, Output: min(5, in.Limits.MaxOutputTokens)}, UpstreamID: uuid.New()}); err != nil {
		return err
	}
	return emit(contracts.ProviderEvent{Type: "done"})
}

// MockTaskProvider simulates bounded media tasks and produces a receipt, never real media.
type MockTaskProvider struct {
	Behavior string
	calls    atomic.Int64
}

// Calls is how many times Invoke was called.
func (m *MockTaskProvider) Calls() int64 { return m.calls.Load() }

// Invoke implements contracts.Provider.
func (m *MockTaskProvider) Invoke(ctx context.Context, in contracts.Invocation, o contracts.Offering, emit func(contracts.ProviderEvent) error) error {
	m.calls.Add(1)
	task := in.Input.Task
	if task == nil || o.TaskKind == nil || task.Kind != *o.TaskKind || m.Behavior == "reject" {
		return &contracts.ProviderError{Definitive: true, Code: "unsupported_task"}
	}
	if ctx.Err() != nil || m.Behavior == "unknown" {
		return &contracts.ProviderError{Code: "usage_unknown"}
	}
	meter := task.MeterName()
	units := in.Limits.MaxUsage[meter]
	if units == 0 || units > o.MaxUsage[meter] {
		return &contracts.ProviderError{Definitive: true, Code: "meter_limit"}
	}
	partial := m.Behavior == "partial"
	completed := units
	if partial {
		completed = units / 2
		if meter == "tasks" {
			completed = 0
		}
	}
	if err := emit(contracts.ProviderEvent{Type: "delta", Text: "Simulated " + task.Kind + " task receipt; no media generated."}); err != nil {
		return err
	}
	usage := contracts.Meters{}
	switch meter {
	case "images":
		usage.Images = &completed
	case "video_ms":
		usage.VideoMs = &completed
	case "audio_ms":
		usage.AudioMs = &completed
	case "tasks":
		usage.Tasks = &completed
	default:
		usage.Units = map[string]int64{meter: completed}
	}
	if err := emit(contracts.ProviderEvent{Type: "usage", Usage: usage, UpstreamID: uuid.New()}); err != nil {
		return err
	}
	done := contracts.ProviderEvent{Type: "done", Completion: "succeeded"}
	if partial {
		done.Completion = "partial"
	}
	return emit(done)
}

// MockToolProvider is a scripted model that calls tools without a network, for demos and tests. It calls the tool
// named by tool_choice (or the first declared tool) with Args, and answers in text once the last message is a tool
// result. Behavior is "success", "truncated" (the call is cut off by the output limit) or "reject".
type MockToolProvider struct {
	Args     map[string]any
	Behavior string
	calls    atomic.Int64
}

var _ contracts.CapableProvider = (*MockToolProvider)(nil)

// Calls is how many times Invoke was called.
func (m *MockToolProvider) Calls() int64 { return m.calls.Load() }

// Capabilities implements contracts.CapableProvider.
func (m *MockToolProvider) Capabilities() contracts.ProviderCapabilities {
	return contracts.ProviderCapabilities{Tools: true, Images: true}
}

// Invoke implements contracts.Provider.
func (m *MockToolProvider) Invoke(ctx context.Context, in contracts.Invocation, _ contracts.Offering, emit func(contracts.ProviderEvent) error) error {
	m.calls.Add(1)
	if m.Behavior == "reject" {
		return &contracts.ProviderError{Definitive: true, Retryable: true, Code: "rate_limited"}
	}
	if ctx.Err() != nil {
		return &contracts.ProviderError{Code: "upstream_failed"}
	}
	usage := contracts.ProviderEvent{Type: "usage", Usage: contracts.Meters{Input: 10, Output: 5}, UpstreamID: uuid.New()}
	messages, tools, choice := in.Input.Messages, in.Input.Tools, in.Input.ToolChoice
	last := contracts.Message{}
	if len(messages) > 0 {
		last = messages[len(messages)-1]
	}
	if tools == nil || choice != nil && choice.Mode == "none" || last.Role == "tool" {
		text := "Hello from SuperRouter."
		if last.Role == "tool" {
			text = "The tool finished."
		}
		if err := emit(contracts.ProviderEvent{Type: "delta", Text: text}); err != nil {
			return err
		}
	} else {
		name := tools[0].Name
		if choice != nil && choice.Name != "" {
			name = choice.Name
		}
		args := m.Args
		if args == nil {
			args = map[string]any{}
		}
		raw, _ := json.Marshal(args)
		middle := (len(raw) + 1) / 2
		id := "call_" + strings.ReplaceAll(uuid.New(), "-", "")
		if err := emit(contracts.ProviderEvent{Type: "tool_call", Index: 0, ID: id, Name: name, ArgumentsDelta: string(raw[:middle])}); err != nil {
			return err
		}
		if m.Behavior == "truncated" {
			if err := emit(usage); err != nil {
				return err
			}
			return emit(contracts.ProviderEvent{Type: "done", Completion: "partial"})
		}
		if err := emit(contracts.ProviderEvent{Type: "tool_call", Index: 0, ArgumentsDelta: string(raw[middle:])}); err != nil {
			return err
		}
	}
	if err := emit(usage); err != nil {
		return err
	}
	return emit(contracts.ProviderEvent{Type: "done"})
}
