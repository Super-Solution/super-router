// SPDX-License-Identifier: Apache-2.0

// Package anthropic is the contracts.Provider for the Anthropic Messages API. It streams text, normalises
// usage into router meters and classifies failures like the OpenAI provider.
package anthropic

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"net/http"
	"strconv"
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

var _ contracts.Provider = (*Provider)(nil)

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
	messages := []contracts.Message{}
	for _, m := range in.Input.Messages {
		if m.Role == "system" {
			system = append(system, m.Content)
		} else {
			messages = append(messages, m)
		}
	}
	if len(messages) == 0 {
		return perr(true, false, "messages_required")
	}
	body, _ := json.Marshal(map[string]any{
		"model": offering.UpstreamModel, "max_tokens": in.Limits.MaxOutputTokens, "system": strings.Join(system, "\n"),
		"messages": messages, "stream": true,
	})
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
		switch res.StatusCode {
		case 400, 401, 403, 404, 422:
			return perr(true, false, "upstream_http_"+strconv.Itoa(res.StatusCode))
		case 429:
			return perr(true, true, "upstream_http_429")
		}
		return perr(false, false, "upstream_http_"+strconv.Itoa(res.StatusCode))
	}
	var (
		current    *usage
		id         string
		terminal   bool
		completion = "succeeded"
	)
	err = sse.Read(res.Body, func(f sse.Frame) error {
		var e struct {
			Type    string `json:"type"`
			Message struct {
				ID    string `json:"id"`
				Usage *usage `json:"usage"`
			} `json:"message"`
			Delta struct {
				Type       string `json:"type"`
				Text       string `json:"text"`
				StopReason string `json:"stop_reason"`
			} `json:"delta"`
			Usage map[string]json.RawMessage `json:"usage"`
		}
		if json.Unmarshal([]byte(f.Data), &e) != nil {
			return perr(false, false, "invalid_upstream_event")
		}
		switch e.Type {
		case "message_start":
			current, id = e.Message.Usage, e.Message.ID
		case "content_block_delta":
			if e.Delta.Type == "text_delta" {
				return emit(contracts.ProviderEvent{Type: "delta", Text: e.Delta.Text})
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
						if json.Unmarshal(raw, &n) != nil {
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
