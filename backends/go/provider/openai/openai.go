// SPDX-License-Identifier: Apache-2.0

// Package openai is the contracts.Provider for the OpenAI Responses API. It streams text, normalises usage
// into router meters and classifies failures: a definitive rejection (the provider certainly did no work) lets
// the router try the next offering, anything uncertain keeps the reservation pending.
package openai

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"net/http"
	"strconv"

	"github.com/Super-Solution/super-router/backends/go/contracts"
	"github.com/Super-Solution/super-router/backends/go/internal/sse"
)

const defaultBase = "https://api.openai.com"

// Provider calls the OpenAI Responses API.
type Provider struct {
	key    func() string
	client *http.Client
	base   string
}

var _ contracts.CapableProvider = (*Provider)(nil)

// Capabilities implements contracts.CapableProvider: tool calling and image input.
func (p *Provider) Capabilities() contracts.ProviderCapabilities {
	return contracts.ProviderCapabilities{Tools: true, Images: true}
}

// Option configures New.
type Option func(*Provider)

// WithHTTPClient sets the client, for proxies, tracing or tests. Redirects are always refused.
func WithHTTPClient(c *http.Client) Option { return func(p *Provider) { p.client = c } }

// WithBaseURL points the provider at an OpenAI-compatible server.
func WithBaseURL(u string) Option { return func(p *Provider) { p.base = u } }

// New creates a provider. key is read on every call, so it can rotate; an empty key is a definitive failure.
func New(key func() string, opts ...Option) *Provider {
	p := &Provider{key: key, base: defaultBase}
	for _, o := range opts {
		o(p)
	}
	client := http.Client{}
	if p.client != nil {
		client = *p.client
	}
	client.CheckRedirect = func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }
	p.client = &client
	return p
}

// usage keeps the two required counters as pointers: a response that omits them must be refused, not billed as
// zero tokens (the TypeScript adapter does the same).
type usage struct {
	InputTokens        *int64 `json:"input_tokens"`
	OutputTokens       *int64 `json:"output_tokens"`
	InputTokensDetails struct {
		CachedTokens     int64 `json:"cached_tokens"`
		CacheWriteTokens int64 `json:"cache_write_tokens"`
	} `json:"input_tokens_details"`
}

// Normalize converts OpenAI usage to router meters. OpenAI counts cached and cache-write tokens inside input.
func (u usage) normalize() (contracts.Meters, bool) {
	if u.InputTokens == nil || u.OutputTokens == nil {
		return contracts.Meters{}, false
	}
	m := contracts.Meters{
		Input:      *u.InputTokens - u.InputTokensDetails.CachedTokens - u.InputTokensDetails.CacheWriteTokens,
		Cached:     u.InputTokensDetails.CachedTokens,
		CacheWrite: u.InputTokensDetails.CacheWriteTokens,
		Output:     *u.OutputTokens,
	}
	return m, m.Valid()
}

func perr(definitive, retryable bool, code string) error {
	return &contracts.ProviderError{Definitive: definitive, Retryable: retryable, Code: code}
}

// Invoke implements contracts.Provider.
func (p *Provider) Invoke(ctx context.Context, in contracts.Invocation, offering contracts.Offering, emit func(contracts.ProviderEvent) error) error {
	if in.Input.Task != nil {
		return perr(true, false, "unsupported_task")
	}
	key := p.key()
	if key == "" {
		return perr(true, false, "credentials_missing")
	}
	fields := map[string]any{
		"model": offering.UpstreamModel, "input": Input(in.Input.Messages), "max_output_tokens": in.Limits.MaxOutputTokens,
		"store": false, "stream": true,
	}
	toolFields(fields, in.Input)
	body, _ := json.Marshal(fields)
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, p.base+"/v1/responses", bytes.NewReader(body))
	if err != nil {
		return perr(true, false, "request_invalid")
	}
	req.Header.Set("Authorization", "Bearer "+key)
	req.Header.Set("Content-Type", "application/json")
	res, err := p.client.Do(req)
	if err != nil {
		return perr(false, false, "transport_unknown")
	}
	defer res.Body.Close()
	if res.StatusCode < 200 || res.StatusCode > 299 {
		switch res.StatusCode {
		case 400, 401, 403, 404, 422:
			return perr(true, false, "upstream_http_"+strconv.Itoa(res.StatusCode))
		case 429:
			return perr(true, true, "upstream_http_429")
		}
		return perr(false, false, "upstream_http_"+strconv.Itoa(res.StatusCode))
	}
	terminal := false
	// Upstream numbers every output item; tool calls are numbered from zero among themselves.
	type call struct {
		index    int
		streamed bool
	}
	calls := map[int]*call{}
	err = sse.Read(res.Body, func(f sse.Frame) error {
		var e struct {
			Type        string          `json:"type"`
			Delta       json.RawMessage `json:"delta"`
			OutputIndex json.RawMessage `json:"output_index"`
			Arguments   json.RawMessage `json:"arguments"`
			Item        struct {
				Type   string          `json:"type"`
				CallID json.RawMessage `json:"call_id"`
				Name   json.RawMessage `json:"name"`
			} `json:"item"`
			Response struct {
				ID    string `json:"id"`
				Usage *usage `json:"usage"`
			} `json:"response"`
		}
		if json.Unmarshal([]byte(f.Data), &e) != nil {
			return perr(false, false, "invalid_upstream_event")
		}
		bad := perr(false, false, "invalid_upstream_event")
		position, positioned := safeIndex(e.OutputIndex)
		switch e.Type {
		case "response.output_text.delta":
			delta, ok := text(e.Delta)
			if !ok && e.Delta != nil && !isNull(e.Delta) {
				return bad
			}
			return emit(contracts.ProviderEvent{Type: "delta", Text: delta})
		case "response.output_item.added":
			if e.Item.Type != "function_call" {
				return nil
			}
			// A call without its id, name or position cannot be answered later: refuse it rather than invent one.
			id, okID := text(e.Item.CallID)
			name, okName := text(e.Item.Name)
			if !okID || !okName || !positioned || calls[position] != nil {
				return bad
			}
			c := &call{index: len(calls)}
			calls[position] = c
			return emit(contracts.ProviderEvent{Type: "tool_call", Index: c.index, ID: id, Name: name})
		case "response.function_call_arguments.delta":
			c := calls[position]
			delta, ok := text(e.Delta)
			if !positioned || c == nil || !ok {
				return bad
			}
			c.streamed = true
			return emit(contracts.ProviderEvent{Type: "tool_call", Index: c.index, ArgumentsDelta: delta})
		case "response.function_call_arguments.done":
			c := calls[position]
			args, ok := text(e.Arguments)
			if !positioned || c == nil || !ok && e.Arguments != nil {
				return bad
			}
			// Some models send the arguments only here.
			if !c.streamed {
				return emit(contracts.ProviderEvent{Type: "tool_call", Index: c.index, ArgumentsDelta: args})
			}
			return nil
		case "response.completed", "response.incomplete":
			if e.Response.Usage == nil {
				return perr(false, false, "usage_unknown")
			}
			meters, ok := e.Response.Usage.normalize()
			if !ok {
				return perr(false, false, "invalid_usage")
			}
			if err := emit(contracts.ProviderEvent{Type: "usage", Usage: meters, UpstreamID: e.Response.ID}); err != nil {
				return err
			}
			terminal = true
			completion := "succeeded"
			if e.Type == "response.incomplete" {
				completion = "partial"
			}
			if err := emit(contracts.ProviderEvent{Type: "done", Completion: completion}); err != nil {
				return err
			}
			return errStop
		case "error", "response.failed":
			return perr(false, false, "upstream_incomplete")
		}
		return nil
	})
	if errors.Is(err, errStop) {
		err = nil
	}
	if err != nil {
		var pe *contracts.ProviderError
		if errors.As(err, &pe) || errors.Is(err, context.Canceled) {
			return err
		}
		if errors.Is(err, sse.ErrFrameTooLarge) {
			return perr(false, false, "stream_frame_too_large")
		}
		return perr(false, false, "stream_interrupted")
	}
	if !terminal {
		return perr(false, false, "stream_interrupted")
	}
	return nil
}

var errStop = errors.New("stop")
