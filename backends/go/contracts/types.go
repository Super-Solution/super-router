// SPDX-License-Identifier: Apache-2.0

// Package contracts holds the wire types shared by every Super Router component in Go: services, offerings,
// invocations, usage, principals and the error types. They mirror packages/contracts of the TypeScript
// reference. Use ParseService and ParseInvocation to decode untrusted JSON with the reference's validation.
package contracts

import (
	"context"
	"encoding/json"

	"github.com/Super-Solution/super-router/backends/go/pricing"
)

// RouterError is a stable, client-visible error code with an HTTP status.
type RouterError = pricing.RouterError

// NewError creates a RouterError.
func NewError(code string, status int) *RouterError { return &RouterError{Code: code, Status: status} }

// Retryable reports whether a client may retry the failed call as is.
func Retryable(err error) bool {
	re, ok := err.(*RouterErrorWithRequest)
	return ok && re.Retryable
}

// RouterErrorWithRequest is a RouterError that also carries the request it belongs to.
type RouterErrorWithRequest struct {
	*RouterError
	RequestID string
	Retryable bool
}

// Error returns the code.
func (e *RouterErrorWithRequest) Error() string { return e.Code }

// Unwrap exposes the underlying RouterError for errors.As.
func (e *RouterErrorWithRequest) Unwrap() error { return e.RouterError }

// ProviderError describes a failed upstream call. Definitive means the provider certainly did no work, so
// the reservation can be released; Retryable means another offering may be tried.
type ProviderError struct {
	Definitive bool
	Retryable  bool
	Code       string
	// RetryAfterMs is how long the provider asked callers to wait before trying again (its Retry-After), in
	// milliseconds; 0 means it asked for nothing. Only a hint: a health book that cools the offering treats it as a
	// minimum, never as a promise.
	RetryAfterMs int64
}

// Error returns the code.
func (e *ProviderError) Error() string { return e.Code }

// Principal is an authenticated caller. Hosts must only construct one from verified credentials.
type Principal struct {
	Workspace   string
	Environment string
	Customer    string
	Scopes      []string
	AuthKeyID   string
	MemberID    string
	// ServiceIDs limits the principal to these services. Nil means unrestricted.
	ServiceIDs []string
}

// HasScope reports whether the principal holds a scope.
func (p Principal) HasScope(scope string) bool {
	for _, s := range p.Scopes {
		if s == scope {
			return true
		}
	}
	return false
}

// MayUse reports whether the principal may touch a service.
func (p Principal) MayUse(service string) bool {
	if p.ServiceIDs == nil {
		return true
	}
	for _, s := range p.ServiceIDs {
		if s == service {
			return true
		}
	}
	return false
}

// QualityMetadata identifies the evaluation behind an offering's quality score.
type QualityMetadata struct {
	Source    string `json:"source"`
	Task      string `json:"task"`
	UpdatedAt string `json:"updated_at"`
}

// LabelEvidence is the provenance and validity of a handling or compliance label.
type LabelEvidence struct {
	Source       string  `json:"source"`
	Verification string  `json:"verification"` // verified or unverified
	ExpiresAt    *string `json:"expires_at,omitempty"`
}

// JSONSchema is an enforced JSON Schema response.
type JSONSchema struct {
	Name        string     `json:"name"`
	Description *string    `json:"description,omitempty"`
	Schema      JSONObject `json:"schema"`
	Strict      *bool      `json:"strict,omitempty"`
}

// ResponseFormat asks for structured output.
type ResponseFormat struct {
	Type       string     `json:"type"` // json_schema
	JSONSchema JSONSchema `json:"json_schema"`
}

// DowngradePolicy explicitly allows supply in an operator-designated lower tier.
type DowngradePolicy struct {
	Tier      string     `json:"tier"`
	Offerings []Offering `json:"offerings"`
}

// MeterLimits are media and custom-unit limits. Media meters sit at the top level of the wire object and custom
// units under "units"; in memory both are one flat map.
type MeterLimits map[string]int64

// MarshalJSON writes media meters at the top level and custom units under "units".
func (m MeterLimits) MarshalJSON() ([]byte, error) {
	out := map[string]any{}
	units := map[string]int64{}
	for k, n := range m {
		if isMedia(k) {
			out[k] = n
		} else {
			units[k] = n
		}
	}
	if len(units) > 0 {
		out["units"] = units
	}
	return json.Marshal(out)
}

// UnmarshalJSON accepts media meters at the top level and custom units under "units", nothing else.
func (m *MeterLimits) UnmarshalJSON(data []byte) error {
	var raw map[string]json.RawMessage
	if err := json.Unmarshal(data, &raw); err != nil || raw == nil {
		return errInvalid
	}
	out := MeterLimits{}
	for k, v := range raw {
		if k == "units" {
			var units map[string]json.RawMessage
			if err := json.Unmarshal(v, &units); err != nil || units == nil {
				return errInvalid
			}
			for name, n := range units {
				var x int64
				if !pricing.ValidUnitName(name) || json.Unmarshal(n, &x) != nil || !safeNonNeg(x) {
					return errInvalid
				}
				out[name] = x
			}
			continue
		}
		var x int64
		if !isMedia(k) || json.Unmarshal(v, &x) != nil || !safeNonNeg(x) {
			return errInvalid
		}
		out[k] = x
	}
	*m = out
	return nil
}

// Offering is one concrete model route a service can use.
type Offering struct {
	Capabilities    *[]string                 `json:"capabilities,omitempty"`
	QualityMetadata *QualityMetadata          `json:"quality_metadata,omitempty"`
	LabelEvidence   *map[string]LabelEvidence `json:"label_evidence,omitempty"`
	ID              string                    `json:"id"`
	TaskKind        *string                   `json:"task_kind,omitempty"`
	MaxUsage        MeterLimits               `json:"max_usage,omitempty"`
	Provider        string                    `json:"provider"`
	Model           string                    `json:"model"`
	UpstreamModel   string                    `json:"upstream_model"`
	Region          string                    `json:"region"`
	Labels          []string                  `json:"labels"`
	Enabled         bool                      `json:"enabled"`
	Source          string                    `json:"source"`
	MaxInputTokens  int64                     `json:"max_input_tokens"`
	MaxOutputTokens int64                     `json:"max_output_tokens"`
	Price           pricing.Price             `json:"price"`
	LatencyMs       *float64                  `json:"latency_ms,omitempty"`
	Reliability     *float64                  `json:"reliability,omitempty"`
	Quality         *float64                  `json:"quality,omitempty"`
}

// HasCapability reports whether the offering declares an optional capability (tools, structured_output).
func (o Offering) HasCapability(name string) bool {
	if o.Capabilities == nil {
		return false
	}
	for _, c := range *o.Capabilities {
		if c == name {
			return true
		}
	}
	return false
}

// Evidence returns the evidence recorded for a label, or nil.
func (o Offering) Evidence(label string) *LabelEvidence {
	if o.LabelEvidence == nil {
		return nil
	}
	e, ok := (*o.LabelEvidence)[label]
	if !ok {
		return nil
	}
	return &e
}

// Weights are the scoring weights; they sum to one.
type Weights struct {
	Cost        float64 `json:"cost"`
	Latency     float64 `json:"latency"`
	Reliability float64 `json:"reliability"`
	Quality     float64 `json:"quality"`
}

// Service is a published, versioned product a developer sells.
type Service struct {
	DowngradePolicy       *DowngradePolicy `json:"downgrade_policy,omitempty"`
	RequireVerifiedLabels *bool            `json:"require_verified_labels,omitempty"`
	ID                    string           `json:"id"`
	Version               int64            `json:"version"`
	TaskKind              *string          `json:"task_kind,omitempty"`
	Mode                  string           `json:"mode"`
	Model                 string           `json:"model,omitempty"`
	Tier                  string           `json:"tier,omitempty"`
	Offerings             []Offering       `json:"offerings"`
	Regions               []string         `json:"regions"`
	RequiredLabels        []string         `json:"required_labels"`
	Price                 pricing.Price    `json:"price"`
	PlatformPrice         *pricing.Price   `json:"platform_price,omitempty"`
	RequiresSubscription  *bool            `json:"requires_subscription,omitempty"`
	MaxAttempts           int64            `json:"max_attempts"`
	MaxProviderCost       string           `json:"max_provider_cost"`
	MaxOutputTokens       int64            `json:"max_output_tokens"`
	Weights               Weights          `json:"weights"`
	CostScale             string           `json:"cost_scale"`
	LatencyScaleMs        float64          `json:"latency_scale_ms"`
	UnknownScore          float64          `json:"unknown_score"`
}

// AllOfferings lists the primary offerings and then the explicitly allowed downgrade ones.
func (s Service) AllOfferings() []Offering {
	all := append([]Offering{}, s.Offerings...)
	if s.DowngradePolicy != nil {
		all = append(all, s.DowngradePolicy.Offerings...)
	}
	return all
}

// Task is a bounded media or tool task instead of chat messages. Meter names a custom unit (or a media meter)
// that bills it instead of the kind's own meter.
type Task struct {
	Meter  string `json:"meter,omitempty"`
	Kind   string `json:"kind"`
	Prompt string `json:"prompt"`
}

// MeterName is the custom meter when set, or the built-in meter of the kind.
func (t Task) MeterName() string {
	if t.Meter != "" {
		return t.Meter
	}
	return KindMeter[t.Kind]
}

// ChargeLimit caps what a customer may be charged for one request.
type ChargeLimit struct {
	Asset string `json:"asset"`
	Atoms string `json:"atoms"`
	Scale int64  `json:"scale"`
}

// Limits bound one invocation.
type Limits struct {
	MaxOutputTokens int64 `json:"max_output_tokens"`
	// MaxUsage is nil when absent: a chat request must not carry usage limits at all, even an empty object.
	MaxUsage  MeterLimits  `json:"max_usage,omitempty"`
	TimeoutMs int64        `json:"timeout_ms"`
	MaxCharge *ChargeLimit `json:"max_charge,omitempty"`
}

// Invocation is one request to run a service.
type Invocation struct {
	ResponseFormat *ResponseFormat `json:"response_format,omitempty"`
	Service        string          `json:"service"`
	ServiceVersion *int64          `json:"service_version,omitempty"`
	CustomerID     string          `json:"customer_id"`
	PayerMode      string          `json:"payer_mode"`
	Input          Input           `json:"input"`
	Limits         Limits          `json:"limits"`
	Stream         bool            `json:"stream"`
}

// Input is what an invocation asks for: chat messages (optionally with tools), or a bounded task. Tools and
// ToolChoice are nil when absent, so a request without tools keeps its exact canonical form and digest.
type Input struct {
	Messages   []Message   `json:"messages"`
	Tools      []Tool      `json:"tools,omitempty"`
	ToolChoice *ToolChoice `json:"tool_choice,omitempty"`
	Task       *Task       `json:"task,omitempty"`
}

// Meters is a usage record reported by a provider. Media meters are nil when not reported.
type Meters struct {
	Units      map[string]int64 `json:"units,omitempty"`
	Input      int64            `json:"input"`
	Cached     int64            `json:"cached"`
	CacheWrite int64            `json:"cache_write"`
	Output     int64            `json:"output"`
	Images     *int64           `json:"images,omitempty"`
	VideoMs    *int64           `json:"video_ms,omitempty"`
	AudioMs    *int64           `json:"audio_ms,omitempty"`
	Tasks      *int64           `json:"tasks,omitempty"`
}

// Usage converts to the pricing representation.
func (m Meters) Usage() pricing.Usage {
	u := pricing.Usage{Input: m.Input, Cached: m.Cached, CacheWrite: m.CacheWrite, Output: m.Output, Media: map[string]int64{}}
	for key, v := range map[string]*int64{"images": m.Images, "video_ms": m.VideoMs, "audio_ms": m.AudioMs, "tasks": m.Tasks} {
		if v != nil {
			u.Media[key] = *v
		}
	}
	if len(m.Units) > 0 {
		u.Units = m.Units
	}
	return u
}

// Quantities are every media meter (zero when not reported) and every custom unit, in one map.
func (m Meters) Quantities() map[string]int64 {
	out := map[string]int64{}
	for _, k := range MediaKeys {
		out[k] = m.Media(k)
	}
	for k, n := range m.Units {
		out[k] = n
	}
	return out
}

// Media returns one media meter, zero when not reported.
func (m Meters) Media(key string) int64 {
	var v *int64
	switch key {
	case "images":
		v = m.Images
	case "video_ms":
		v = m.VideoMs
	case "audio_ms":
		v = m.AudioMs
	case "tasks":
		v = m.Tasks
	}
	if v == nil {
		return 0
	}
	return *v
}

// ProviderEvent is one step of a provider's answer: a text delta, part of a tool call, the final usage, or the end.
type ProviderEvent struct {
	Type       string // "delta", "tool_call", "usage" or "done"
	Text       string
	Usage      Meters
	UpstreamID string
	Completion string // for "done": "succeeded" (default), "partial", or "filtered" (cut short by the provider's content policy: recorded as partial, reported as finish reason content_filter)
	// For "tool_call": calls are numbered from zero by Index. The first event of an index carries ID and Name (and
	// Signature, when the upstream attached one); ArgumentsDelta is the next piece of the call's JSON arguments.
	Index          int
	ID             string
	Name           string
	Signature      string
	ArgumentsDelta string
}

// Provider runs one invocation against an upstream model, reporting events through emit as they arrive.
// It returns nil after the "done" event, or a *ProviderError (or any error, treated as an unknown outcome).
// Implementations must stop when ctx is cancelled and must return promptly if emit returns an error.
type Provider interface {
	Invoke(ctx context.Context, in Invocation, offering Offering, emit func(ProviderEvent) error) error
}

// StreamEvent is one server-sent event of a streaming inference.
type StreamEvent struct {
	Type      string
	RequestID string
	Sequence  int
	Data      map[string]any
}

// RoleScopes maps each member role to the scopes its keys may hold.
var RoleScopes = map[string][]string{
	"owner":         {"invoke", "identity:write", "service:write", "routing:write", "billing:write", "billing:read", "operations:read"},
	"service_admin": {"invoke", "service:write", "routing:write", "operations:read"},
	"billing_admin": {"billing:write", "billing:read", "operations:read"},
	"viewer":        {"billing:read", "operations:read"},
	"app":           {"invoke", "credits:read"},
}
