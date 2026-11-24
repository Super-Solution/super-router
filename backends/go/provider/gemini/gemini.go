// SPDX-License-Identifier: Apache-2.0

// Package gemini is the contracts.Provider for Google's Gemini generateContent API. It streams text and tool calls,
// normalises usageMetadata into router meters and classifies failures like the OpenAI and Anthropic providers.
//
// The same wire format is served by the Gemini API (API key header, /v1beta) and by Vertex AI (OAuth bearer token, a
// project and location in the path), so one provider covers both: see WithPathPrefix and WithBearerAuth.
package gemini

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"io"
	"net/http"
	"strconv"
	"strings"

	"github.com/Super-Solution/super-router/backends/go/contracts"
	"github.com/Super-Solution/super-router/backends/go/internal/sse"
)

const (
	defaultBase   = "https://generativelanguage.googleapis.com"
	defaultPrefix = "/v1beta"
)

// Provider calls the Gemini generateContent API.
type Provider struct {
	key    func() string
	client *http.Client
	base   string
	prefix string
	bearer bool
}

var _ contracts.CapableProvider = (*Provider)(nil)

// Capabilities implements contracts.CapableProvider: tool calling and image input (inline data only).
func (p *Provider) Capabilities() contracts.ProviderCapabilities {
	return contracts.ProviderCapabilities{Tools: true, Images: true}
}

// Option configures New.
type Option func(*Provider)

// WithHTTPClient sets the client, for proxies, tracing or tests. Redirects are always refused.
func WithHTTPClient(c *http.Client) Option { return func(p *Provider) { p.client = c } }

// WithBaseURL points the provider at another server (a proxy, a regional Vertex endpoint, a test server).
func WithBaseURL(u string) Option { return func(p *Provider) { p.base = u } }

// WithPathPrefix sets what goes between the host and /models/<model>:streamGenerateContent. The default is /v1beta;
// Vertex AI uses /v1/projects/<project>/locations/<location>/publishers/google.
func WithPathPrefix(prefix string) Option { return func(p *Provider) { p.prefix = prefix } }

// WithBearerAuth sends the credential as `Authorization: Bearer ...` (Vertex AI access tokens) instead of
// `x-goog-api-key`.
func WithBearerAuth() Option { return func(p *Provider) { p.bearer = true } }

// New creates a provider. key is read on every call, so it can rotate; an empty key is a definitive failure.
func New(key func() string, opts ...Option) *Provider {
	p := &Provider{key: key, base: defaultBase, prefix: defaultPrefix}
	for _, o := range opts {
		o(p)
	}
	p.base = strings.TrimRight(p.base, "/")
	p.prefix = strings.TrimRight(p.prefix, "/")
	client := http.Client{}
	if p.client != nil {
		client = *p.client
	}
	client.CheckRedirect = func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }
	p.client = &client
	return p
}

func perr(definitive, retryable bool, code string) error {
	return &contracts.ProviderError{Definitive: definitive, Retryable: retryable, Code: code}
}

// modelSegment is the model name as it goes into the path: without a `models/` prefix, and only characters that
// cannot change the path or the query.
func modelSegment(upstreamModel string) (string, bool) {
	name := strings.TrimPrefix(upstreamModel, "models/")
	if name == "" {
		return "", false
	}
	for i := 0; i < len(name); i++ {
		b := name[i]
		if !(b >= 'a' && b <= 'z' || b >= 'A' && b <= 'Z' || b >= '0' && b <= '9' || b == '.' || b == '-' || b == '_') {
			return "", false
		}
	}
	return name, true
}

type functionCall struct {
	ID   *string         `json:"id"`
	Name *string         `json:"name"`
	Args json.RawMessage `json:"args"`
}

type part struct {
	Text         *string       `json:"text"`
	Thought      *bool         `json:"thought"`
	FunctionCall *functionCall `json:"functionCall"`
	// ThoughtSignature is Gemini 3's opaque signature of its reasoning; on a function call it must come back when
	// the call is replayed. It is passed on untouched as the call's Signature.
	ThoughtSignature *string `json:"thoughtSignature"`
}

type chunk struct {
	Candidates []struct {
		Content *struct {
			Parts []part `json:"parts"`
		} `json:"content"`
		FinishReason *string `json:"finishReason"`
	} `json:"candidates"`
	UsageMetadata  *UsageMetadata `json:"usageMetadata"`
	ResponseID     *string        `json:"responseId"`
	PromptFeedback *struct {
		BlockReason *string `json:"blockReason"`
	} `json:"promptFeedback"`
	Error json.RawMessage `json:"error"`
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
	model, ok := modelSegment(offering.UpstreamModel)
	if !ok {
		return perr(true, false, "request_invalid")
	}
	var system []string
	for _, m := range in.Input.Messages {
		if m.Role == "system" {
			system = append(system, m.Content)
		}
	}
	turns, err := contents(in.Input.Messages)
	if err != nil {
		return perr(true, false, "request_invalid")
	}
	if len(turns) == 0 {
		return perr(true, false, "messages_required")
	}
	fields := map[string]any{"contents": turns, "generationConfig": map[string]any{"maxOutputTokens": in.Limits.MaxOutputTokens}}
	// Omitted rather than sent empty when there is no system message.
	if joined := strings.Join(system, "\n"); joined != "" {
		fields["systemInstruction"] = map[string]any{"parts": []any{map[string]any{"text": joined}}}
	}
	toolFields(fields, in.Input)
	body, err := json.Marshal(fields)
	if err != nil {
		return perr(true, false, "request_invalid")
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, p.base+p.prefix+"/models/"+model+":streamGenerateContent?alt=sse", bytes.NewReader(body))
	if err != nil {
		return perr(true, false, "request_invalid")
	}
	// A credential that cannot be a header value never leaves the process, so nothing was dispatched.
	if strings.ContainsAny(key, "\r\n\x00") {
		return perr(true, false, "request_invalid")
	}
	if p.bearer {
		req.Header.Set("Authorization", "Bearer "+key)
	} else {
		req.Header.Set("x-goog-api-key", key)
	}
	req.Header.Set("Content-Type", "application/json")
	res, err := p.client.Do(req)
	if err != nil {
		return perr(false, false, "transport_unknown")
	}
	defer res.Body.Close()
	if res.StatusCode < 200 || res.StatusCode > 299 {
		if res.StatusCode == 400 && namesRejectedSignature(res.Body) {
			return perr(true, false, "thought_signature_rejected")
		}
		switch res.StatusCode {
		case 400, 401, 403, 404, 422:
			return perr(true, false, "upstream_http_"+strconv.Itoa(res.StatusCode))
		case 429:
			return perr(true, true, "upstream_http_429")
		}
		return perr(false, false, "upstream_http_"+strconv.Itoa(res.StatusCode))
	}
	var (
		upstreamID string
		usage      *UsageMetadata
		finish     string
		calls      int
	)
	err = sse.Read(res.Body, func(f sse.Frame) error {
		bad := perr(false, false, "invalid_upstream_event")
		var c chunk
		if !strings.HasPrefix(strings.TrimSpace(f.Data), "{") || json.Unmarshal([]byte(f.Data), &c) != nil {
			return bad
		}
		if len(c.Error) > 0 && !bytes.Equal(bytes.TrimSpace(c.Error), []byte("null")) {
			return perr(false, false, "upstream_incomplete")
		}
		if upstreamID == "" && c.ResponseID != nil {
			upstreamID = *c.ResponseID
		}
		if c.UsageMetadata != nil {
			usage = c.UsageMetadata
		}
		if len(c.Candidates) > 0 {
			cand := c.Candidates[0]
			var parts []part
			if cand.Content != nil {
				parts = cand.Content.Parts
			}
			for _, pt := range parts {
				if pt.FunctionCall != nil {
					// A call without its name cannot be answered later: refuse it rather than invent one.
					if pt.FunctionCall.Name == nil || *pt.FunctionCall.Name == "" {
						return bad
					}
					args := map[string]any{}
					if raw := bytes.TrimSpace(pt.FunctionCall.Args); len(raw) > 0 && !bytes.Equal(raw, []byte("null")) {
						dec := json.NewDecoder(bytes.NewReader(raw))
						dec.UseNumber()
						if dec.Decode(&args) != nil || args == nil {
							return bad
						}
					}
					id := ""
					if pt.FunctionCall.ID != nil {
						id = *pt.FunctionCall.ID
					}
					if id == "" {
						stem := upstreamID
						if stem == "" {
							stem = "call"
						}
						id = stem + "_" + strconv.Itoa(calls)
					}
					text, err := compactArgs(args)
					if err != nil {
						return bad
					}
					signature := ""
					if pt.ThoughtSignature != nil {
						signature = *pt.ThoughtSignature
					}
					if err := emit(contracts.ProviderEvent{Type: "tool_call", Index: calls, ID: id, Name: *pt.FunctionCall.Name, Signature: signature, ArgumentsDelta: text}); err != nil {
						return err
					}
					calls++
				} else if (pt.Thought == nil || !*pt.Thought) && pt.Text != nil && *pt.Text != "" {
					if err := emit(contracts.ProviderEvent{Type: "delta", Text: *pt.Text}); err != nil {
						return err
					}
				}
			}
			if cand.FinishReason != nil && *cand.FinishReason != "" {
				finish = *cand.FinishReason
			}
		}
		// A blocked prompt has no candidate and no finish reason; it still ends the call.
		if finish == "" && c.PromptFeedback != nil && c.PromptFeedback.BlockReason != nil {
			finish = "PROMPT_BLOCKED"
		}
		return nil
	})
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
	// The server closes the stream after its last chunk, which is the one that carries the final usage.
	if finish == "" {
		return perr(false, false, "stream_interrupted")
	}
	if usage == nil {
		return perr(false, false, "usage_unknown")
	}
	meters, ok := Normalize(*usage)
	if !ok {
		return perr(false, false, "invalid_usage")
	}
	if err := emit(contracts.ProviderEvent{Type: "usage", Usage: meters, UpstreamID: upstreamID}); err != nil {
		return err
	}
	// Anything but a natural stop (length, safety, recitation, a blocked prompt) cut the answer short.
	completion := "partial"
	if finish == "STOP" {
		completion = "succeeded"
	}
	return emit(contracts.ProviderEvent{Type: "done", Completion: completion})
}

// namesRejectedSignature reports whether a 400 body says a replayed thought signature was refused. A signature only
// validates on the backend that minted it: the Gemini API answers a replayed Vertex signature with `400 Corrupted
// thought signature`, and Vertex answers the Gemini API's with `400 Invalid thought signature`. Telling that apart
// from any other 400 lets the caller see why a conversation cannot move to this offering. Only the error's own
// message is read, from the first 16 KiB; nothing of it is passed on.
func namesRejectedSignature(body io.Reader) bool {
	raw, _ := io.ReadAll(io.LimitReader(body, 16384))
	var e struct {
		Error struct {
			Message string `json:"message"`
		} `json:"error"`
	}
	if json.Unmarshal(raw, &e) != nil {
		return false
	}
	m := strings.ReplaceAll(strings.ToLower(e.Error.Message), "_", " ")
	return strings.Contains(m, "thought signature") || strings.Contains(m, "thoughtsignature")
}
