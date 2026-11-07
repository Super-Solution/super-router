// SPDX-License-Identifier: Apache-2.0

// Package contracts holds the wire types shared by every Super Router component in Go: services, offerings,
// invocations, usage, principals and the error types. They mirror packages/contracts of the TypeScript
// reference. Use ParseService and ParseInvocation to decode untrusted JSON with the reference's validation.
package contracts

import (
	"context"

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

// Offering is one concrete model route a service can use.
type Offering struct {
	ID              string           `json:"id"`
	TaskKind        *string          `json:"task_kind,omitempty"`
	MaxUsage        map[string]int64 `json:"max_usage,omitempty"`
	Provider        string           `json:"provider"`
	Model           string           `json:"model"`
	UpstreamModel   string           `json:"upstream_model"`
	Region          string           `json:"region"`
	Labels          []string         `json:"labels"`
	Enabled         bool             `json:"enabled"`
	Source          string           `json:"source"`
	MaxInputTokens  int64            `json:"max_input_tokens"`
	MaxOutputTokens int64            `json:"max_output_tokens"`
	Price           pricing.Price    `json:"price"`
	LatencyMs       *float64         `json:"latency_ms,omitempty"`
	Reliability     *float64         `json:"reliability,omitempty"`
	Quality         *float64         `json:"quality,omitempty"`
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
	ID                   string         `json:"id"`
	Version              int64          `json:"version"`
	TaskKind             *string        `json:"task_kind,omitempty"`
	Mode                 string         `json:"mode"`
	Model                string         `json:"model,omitempty"`
	Tier                 string         `json:"tier,omitempty"`
	Offerings            []Offering     `json:"offerings"`
	Regions              []string       `json:"regions"`
	RequiredLabels       []string       `json:"required_labels"`
	Price                pricing.Price  `json:"price"`
	PlatformPrice        *pricing.Price `json:"platform_price,omitempty"`
	RequiresSubscription *bool          `json:"requires_subscription,omitempty"`
	MaxAttempts          int64          `json:"max_attempts"`
	MaxProviderCost      string         `json:"max_provider_cost"`
	MaxOutputTokens      int64          `json:"max_output_tokens"`
	Weights              Weights        `json:"weights"`
	CostScale            string         `json:"cost_scale"`
	LatencyScaleMs       float64        `json:"latency_scale_ms"`
	UnknownScore         float64        `json:"unknown_score"`
}

// Message is one chat message.
type Message struct {
	Role    string `json:"role"`
	Content string `json:"content"`
}

// Task is a bounded media or tool task instead of chat messages.
type Task struct {
	Kind   string `json:"kind"`
	Prompt string `json:"prompt"`
}

// ChargeLimit caps what a customer may be charged for one request.
type ChargeLimit struct {
	Asset string `json:"asset"`
	Atoms string `json:"atoms"`
	Scale int64  `json:"scale"`
}

// Limits bound one invocation.
type Limits struct {
	MaxOutputTokens int64            `json:"max_output_tokens"`
	MaxUsage        map[string]int64 `json:"max_usage,omitempty"`
	TimeoutMs       int64            `json:"timeout_ms"`
	MaxCharge       *ChargeLimit     `json:"max_charge,omitempty"`
}

// Invocation is one request to run a service.
type Invocation struct {
	Service        string `json:"service"`
	ServiceVersion *int64 `json:"service_version,omitempty"`
	CustomerID     string `json:"customer_id"`
	PayerMode      string `json:"payer_mode"`
	Input          struct {
		Messages []Message `json:"messages"`
		Task     *Task     `json:"task,omitempty"`
	} `json:"input"`
	Limits Limits `json:"limits"`
	Stream bool   `json:"stream"`
}

// Meters is a usage record reported by a provider. Media meters are nil when not reported.
type Meters struct {
	Input      int64  `json:"input"`
	Cached     int64  `json:"cached"`
	CacheWrite int64  `json:"cache_write"`
	Output     int64  `json:"output"`
	Images     *int64 `json:"images,omitempty"`
	VideoMs    *int64 `json:"video_ms,omitempty"`
	AudioMs    *int64 `json:"audio_ms,omitempty"`
	Tasks      *int64 `json:"tasks,omitempty"`
}

// Usage converts to the pricing representation.
func (m Meters) Usage() pricing.Usage {
	u := pricing.Usage{Input: m.Input, Cached: m.Cached, CacheWrite: m.CacheWrite, Output: m.Output, Media: map[string]int64{}}
	for key, v := range map[string]*int64{"images": m.Images, "video_ms": m.VideoMs, "audio_ms": m.AudioMs, "tasks": m.Tasks} {
		if v != nil {
			u.Media[key] = *v
		}
	}
	return u
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

// ProviderEvent is one step of a provider's answer: a text delta, the final usage, or the end.
type ProviderEvent struct {
	Type       string // "delta", "usage" or "done"
	Text       string
	Usage      Meters
	UpstreamID string
	Completion string // for "done": "succeeded" (default) or "partial"
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
