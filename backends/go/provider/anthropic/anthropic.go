// SPDX-License-Identifier: Apache-2.0

// Package anthropic is the contracts.Provider for the Anthropic Messages API. It streams text, normalises
// usage into router meters and classifies failures like the OpenAI provider.
package anthropic

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"io"
	"net/http"
	"strings"

	"github.com/Super-Solution/super-router/backends/go/contracts"
	"github.com/Super-Solution/super-router/backends/go/internal/sse"
)

const defaultBase = "https://api.anthropic.com"

// Provider calls the Anthropic Messages API.
type Provider struct {
	key    func() string
	client *http.Client
	base   string
}

var _ contracts.CapableProvider = (*Provider)(nil)

// Capabilities implements contracts.CapableProvider: tool calling and image input.
func (p *Provider) Capabilities() contracts.ProviderCapabilities {
	// The Messages API call here has no seed and no reasoning effort: a request that asks for them is refused before reserving.
	return contracts.ProviderCapabilities{Tools: true, Images: true, Temperature: true, TopP: true, Stop: true}
}

// Option configures New.
type Option func(*Provider)

// WithHTTPClient sets the client, for proxies, tracing or tests. Redirects are always refused.
func WithHTTPClient(c *http.Client) Option { return func(p *Provider) { p.client = c } }

// WithBaseURL points the provider at an Anthropic-compatible server.
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
	InputTokens              *int64 `json:"input_tokens"`
	OutputTokens             *int64 `json:"output_tokens"`
	CacheReadInputTokens     int64  `json:"cache_read_input_tokens"`
	CacheCreationInputTokens int64  `json:"cache_creation_input_tokens"`
}

// counter decodes one usage counter. json.Unmarshal accepts null and leaves the zero value without an error, so a
// null counter would silently replace a real count with 0 and bill the call as free. It is refused, like any value
// that is not a JSON integer.
func counter(raw json.RawMessage, n *int64) bool {
	return !bytes.Equal(bytes.TrimSpace(raw), []byte("null")) && json.Unmarshal(raw, n) == nil
}

func perr(definitive, retryable bool, code string) error {
	return &contracts.ProviderError{Definitive: definitive, Retryable: retryable, Code: code}
}

var errStop = errors.New("stop")

// Invoke implements contracts.Provider.
func (p *Provider) Invoke(ctx context.Context, in contracts.Invocation, offering contracts.Offering, emit func(contracts.ProviderEvent) error) error {
	if in.Input.Task != nil {
		return perr(true, false, "unsupported_task")
	}
	key := p.key()
	if key == "" {
		return perr(true, false, "credentials_missing")
	}
	var system []string
	for _, m := range in.Input.Messages {
		if m.Role == "system" {
			system = append(system, m.Content)
		}
	}
	messages := Messages(in.Input.Messages)
	if len(messages) == 0 {
		return perr(true, false, "messages_required")
	}
	fields := map[string]any{"model": offering.UpstreamModel, "max_tokens": in.Limits.MaxOutputTokens, "messages": messages, "stream": true}
	// Omitted rather than sent empty when there is no system message.
	if joined := strings.Join(system, "\n"); joined != "" {
		fields["system"] = joined
	}
	toolFields(fields, in.Input)
	if sm := in.Sampling; sm != nil {
		if sm.Temperature != nil {
			fields["temperature"] = *sm.Temperature
		}
		if sm.TopP != nil {
			fields["top_p"] = *sm.TopP
		}
		if sm.Stop != nil {
			fields["stop_sequences"] = sm.Stop
		}
	}
	// Automatic prompt caching (5 minutes, written at 1.25 times the input price): only when asked for. Absent and "none"
	// send nothing, so nothing is cached and no write is billed. The Anthropic API only: Amazon Bedrock refuses it.
	if in.Cache != nil && in.Cache.Mode == "auto" {
		fields["cache_control"] = map[string]string{"type": "ephemeral"}
	}
	body, err := json.Marshal(fields)
	if err != nil {
		return perr(true, false, "request_invalid") // tool-call arguments that are not JSON, from a hand-built invocation
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, p.base+"/v1/messages", bytes.NewReader(body))
	if err != nil {
		return perr(true, false, "request_invalid")
	}
	req.Header.Set("x-api-key", key)
	req.Header.Set("anthropic-version", "2023-06-01")
	req.Header.Set("Content-Type", "application/json")
	res, err := p.client.Do(req)
	if err != nil {
		return perr(false, false, "transport_unknown")
	}
	defer res.Body.Close()
	if res.StatusCode < 200 || res.StatusCode > 299 {
		var body []byte
		if res.StatusCode >= 400 && res.StatusCode < 500 {
			body, _ = io.ReadAll(io.LimitReader(res.Body, contracts.RefusalBodyLimit))
		}
		return contracts.RefusalFrom(res.StatusCode, res.Header.Get("Retry-After"), body)
	}
	var (
		current    *usage
		id         string
		terminal   bool
		completion = "succeeded"
	)
	// Upstream numbers every content block; tool calls are numbered from zero among themselves.
	type call struct {
		index    int
		streamed bool
		input    json.RawMessage
	}
	calls := map[int]*call{}
	err = sse.Read(res.Body, func(f sse.Frame) error {
		var e struct {
			Type    string `json:"type"`
			Message struct {
				ID    string `json:"id"`
				Usage *usage `json:"usage"`
			} `json:"message"`
			Index        json.RawMessage `json:"index"`
			ContentBlock struct {
				Type  string          `json:"type"`
				ID    json.RawMessage `json:"id"`
				Name  json.RawMessage `json:"name"`
				Input json.RawMessage `json:"input"`
			} `json:"content_block"`
			Delta struct {
				Type        string          `json:"type"`
				Text        string          `json:"text"`
				PartialJSON json.RawMessage `json:"partial_json"`
				StopReason  string          `json:"stop_reason"`
			} `json:"delta"`
			Usage map[string]json.RawMessage `json:"usage"`
		}
		if json.Unmarshal([]byte(f.Data), &e) != nil {
			return perr(false, false, "invalid_upstream_event")
		}
		bad := perr(false, false, "invalid_upstream_event")
		position, positioned := safeIndex(e.Index)
		switch e.Type {
		case "message_start":
			current, id = e.Message.Usage, e.Message.ID
		case "content_block_start":
			if e.ContentBlock.Type != "tool_use" {
				return nil
			}
			// A call without its id, name or position cannot be answered later: refuse it rather than invent one.
			callID, okID := text(e.ContentBlock.ID)
			name, okName := text(e.ContentBlock.Name)
			if !okID || !okName || !positioned || calls[position] != nil {
				return bad
			}
			c := &call{index: len(calls), input: e.ContentBlock.Input}
			calls[position] = c
			return emit(contracts.ProviderEvent{Type: "tool_call", Index: c.index, ID: callID, Name: name})
		case "content_block_stop":
			c := calls[position]
			// A block may carry its whole input at the start and stream nothing.
			if !positioned || c == nil || c.streamed {
				return nil
			}
			if args, ok := wholeInput(c.input); ok {
				return emit(contracts.ProviderEvent{Type: "tool_call", Index: c.index, ArgumentsDelta: args})
			}
		case "content_block_delta":
			switch e.Delta.Type {
			case "text_delta":
				return emit(contracts.ProviderEvent{Type: "delta", Text: e.Delta.Text})
			case "input_json_delta":
				c := calls[position]
				partial, ok := text(e.Delta.PartialJSON)
				if !positioned || c == nil || !ok {
					return bad
				}
				c.streamed = true
				return emit(contracts.ProviderEvent{Type: "tool_call", Index: c.index, ArgumentsDelta: partial})
			}
		case "message_delta":
			if current != nil {
				// message_delta carries the final counters; they overwrite what message_start reported.
				fields := map[string]*int64{
					"cache_read_input_tokens": &current.CacheReadInputTokens, "cache_creation_input_tokens": &current.CacheCreationInputTokens,
				}
				for k, raw := range e.Usage {
					var n int64
					if k == "input_tokens" || k == "output_tokens" || fields[k] != nil {
						if !counter(raw, &n) {
							return perr(false, false, "invalid_usage")
						}
					}
					switch k {
					case "input_tokens":
						current.InputTokens = &n
					case "output_tokens":
						current.OutputTokens = &n
					default:
						if target := fields[k]; target != nil {
							*target = n
						}
					}
				}
				if e.Delta.StopReason == "max_tokens" {
					completion = "partial"
				}
			}
		case "message_stop":
			if current == nil {
				return perr(false, false, "usage_unknown")
			}
			if current.InputTokens == nil || current.OutputTokens == nil {
				return perr(false, false, "invalid_usage")
			}
			meters := contracts.Meters{Input: *current.InputTokens, Cached: current.CacheReadInputTokens, CacheWrite: current.CacheCreationInputTokens, Output: *current.OutputTokens}
			if !meters.Valid() {
				return perr(false, false, "invalid_usage")
			}
			if err := emit(contracts.ProviderEvent{Type: "usage", Usage: meters, UpstreamID: id}); err != nil {
				return err
			}
			terminal = true
			if err := emit(contracts.ProviderEvent{Type: "done", Completion: completion}); err != nil {
				return err
			}
			return errStop
		case "error":
			return perr(false, false, "upstream_stream_error")
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
