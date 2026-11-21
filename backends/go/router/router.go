// SPDX-License-Identifier: Apache-2.0

// Package router is the Super Router data plane: it admits an invocation idempotently, picks offerings,
// reserves the customer's funds, runs the provider and settles or releases the reservation.
//
// It mirrors Router.infer of the TypeScript reference. Storage is the Store port, providers are the
// contracts.Provider port, and nothing here touches HTTP, so any framework or transport can host it.
// An uncertain provider outcome is never refunded: the reservation stays pending for reconciliation.
package router

import (
	"context"
	"crypto/hmac"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"math/big"
	"sort"
	"sync"
	"time"

	"github.com/Super-Solution/super-router/backends/go/contracts"
	"github.com/Super-Solution/super-router/backends/go/pricing"
	"github.com/Super-Solution/super-router/backends/go/routing"
)

// Router runs invocations against a Store and a set of providers. It is safe for concurrent use.
type Router struct {
	store     Store
	providers map[string]contracts.Provider
	digestKey []byte
	clock     Clock
}

// Option configures New.
type Option func(*Router)

// WithClock replaces the wall clock, for tests.
func WithClock(c Clock) Option { return func(r *Router) { r.clock = c } }

// New creates a router. digestKey keys the idempotency digest and must be at least 32 bytes.
func New(store Store, providers map[string]contracts.Provider, digestKey string, opts ...Option) (*Router, error) {
	if len(digestKey) < 32 {
		return nil, errors.New("router: digest key must be at least 32 characters")
	}
	r := &Router{store: store, providers: providers, digestKey: []byte(digestKey), clock: time.Now}
	for _, o := range opts {
		o(r)
	}
	return r, nil
}

// Store returns the router's storage, for hosts that expose balances or cancellation next to inference.
func (r *Router) Store() Store { return r.store }

// Output is the assistant's answer. ToolCalls holds the calls of a finished answer only: a call cut off by a
// limit or a failure is never reported, so nothing incomplete can be executed.
type Output struct {
	Role      string               `json:"role"`
	Content   string               `json:"content"`
	ToolCalls []contracts.ToolCall `json:"tool_calls,omitempty"`
}

// UsageReport tells the caller whether usage is known.
type UsageReport struct {
	Status string            `json:"status"` // confirmed, pending or not_billable
	Meters *contracts.Meters `json:"meters,omitempty"`
}

// Charge is what one account is charged.
type Charge struct {
	Kind  string `json:"kind"`
	Asset string `json:"asset"`
	Atoms string `json:"atoms"`
	Scale int    `json:"scale"`
}

// BillingReport lists the charges of a settled request, empty otherwise.
type BillingReport struct {
	Charges []Charge `json:"charges"`
}

// Result is the answer to Infer. A replayed idempotency key returns only the identifiers and statuses.
type Result struct {
	Replayed        bool           `json:"replayed,omitempty"`
	ID              string         `json:"id"`
	ExecutionStatus string         `json:"execution_status"`
	BillingStatus   string         `json:"billing_status"`
	ServiceVersion  *int64         `json:"service_version,omitempty"`
	Model           string         `json:"model,omitempty"`
	Tier            string         `json:"tier,omitempty"`
	Output          *Output        `json:"output,omitempty"`
	FinishReason    string         `json:"finish_reason,omitempty"` // stop, length or tool_calls
	Usage           *UsageReport   `json:"usage,omitempty"`
	Billing         *BillingReport `json:"billing,omitempty"`
}

func fail(code string, status int) error { return contracts.NewError(code, status) }

func (r *Router) digest(in contracts.Invocation) (string, error) {
	canon, err := contracts.Canonical(in)
	if err != nil {
		return "", err
	}
	mac := hmac.New(sha256.New, r.digestKey)
	mac.Write([]byte(canon))
	return hex.EncodeToString(mac.Sum(nil)), nil
}

func replayed(rec *RequestRecord) *Result {
	return &Result{Replayed: true, ID: rec.ID, ExecutionStatus: rec.Execution, BillingStatus: rec.Billing}
}

// validateShape applies the cross-field rules of an invocation: chat has messages and no media limits, a
// task has no messages and exactly its own meter limited.
func validateShape(in contracts.Invocation) error {
	if task := in.Input.Task; task != nil {
		meter := contracts.KindMeter[task.Kind]
		limits := in.Limits.MaxUsage
		if len(in.Input.Messages) > 0 || limits[meter] == 0 {
			return fail("invalid_task_limits", 400)
		}
		for _, k := range contracts.MediaKeys {
			if k != meter && limits[k] != 0 {
				return fail("invalid_task_limits", 400)
			}
		}
		if meter == "tasks" && limits["tasks"] != 1 {
			return fail("invalid_task_limits", 400)
		}
		return nil
	}
	if len(in.Input.Messages) == 0 || in.Limits.MaxUsage != nil {
		return fail("invalid_request", 400)
	}
	return nil
}

// Infer runs one invocation. raw is the JSON request body; key is the idempotency key. emit, when set,
// receives the stream events in order; if it returns an error the provider is stopped and the outcome is
// recorded as uncertain. Cancelling ctx aborts the provider call but never the settlement.
//
// Errors before funds are reserved are plain *contracts.RouterError. Later errors are
// *contracts.RouterErrorWithRequest carrying the request id.
func (r *Router) Infer(ctx context.Context, p contracts.Principal, raw []byte, key string, emit func(contracts.StreamEvent) error) (*Result, error) {
	if !p.HasScope("invoke") {
		return nil, fail("scope_denied", 403)
	}
	in, err := contracts.ParseInvocation(raw)
	if err != nil {
		return nil, err
	}
	if err := validateShape(in); err != nil {
		return nil, err
	}
	if !p.MayUse(in.Service) || in.CustomerID != p.Customer {
		return nil, fail("scope_denied", 403)
	}
	if key == "" || len(key) > 200 {
		return nil, fail("missing_idempotency_key", 400)
	}
	digest, err := r.digest(in)
	if err != nil {
		return nil, err
	}
	if existing, err := r.store.Replay(ctx, p, key, digest); err != nil {
		return nil, err
	} else if existing != nil {
		return replayed(existing), nil
	}
	service, err := r.store.GetService(ctx, p, in.Service, in.ServiceVersion)
	if err != nil {
		return nil, err
	}
	if in.ServiceVersion != nil {
		active, err := r.store.GetService(ctx, p, in.Service, nil)
		if err != nil {
			return nil, err
		}
		if active.Version != service.Version {
			return nil, fail("inactive_service_version", 409)
		}
	}
	if in.Limits.MaxOutputTokens > service.MaxOutputTokens {
		return nil, fail("unsupported_capability", 422)
	}
	candidates, excluded, err := routing.Route(service, in)
	if err != nil {
		return nil, err
	}
	if len(candidates) == 0 {
		return nil, fail("no_eligible_offering", 422)
	}
	for _, c := range candidates {
		if r.providers[c.Offering.Provider] == nil {
			return nil, fail("provider_unavailable", 503)
		}
	}
	// Refused here, before anything is reserved: a provider that cannot carry tools or images would drop them.
	needs := contracts.InvocationNeeds(in)
	capable := candidates[:0:0]
	for _, c := range candidates {
		if contracts.CapabilitiesOf(r.providers[c.Offering.Provider]).Can(needs) {
			capable = append(capable, c)
		} else {
			excluded = append(excluded, routing.Excluded{ID: c.Offering.ID, Reason: "capability_unsupported"})
		}
	}
	candidates = capable
	if len(candidates) == 0 {
		return nil, fail("unsupported_capability", 422)
	}
	var maxInput int64
	decision := Decision{Excluded: excluded, Candidates: make([]DecisionCandidate, 0, len(candidates))}
	for _, c := range candidates {
		maxInput = max(maxInput, c.Offering.MaxInputTokens)
		decision.Candidates = append(decision.Candidates, DecisionCandidate{ID: c.Offering.ID, Score: c.Score, Cost: c.Cost.String()})
	}
	legs, err := r.store.Accounts(ctx, p, service)
	if err != nil {
		return nil, err
	}
	for i := range legs {
		bound, err := routing.InvocationBound(in, maxInput, legs[i].Price)
		if err != nil {
			return nil, err
		}
		legs[i].Reserved = bound.String()
		if mc := in.Limits.MaxCharge; legs[i].Kind == "customer_sale" && mc != nil {
			limit, _ := new(big.Int).SetString(mc.Atoms, 10)
			if mc.Asset != legs[i].Price.Asset || limit.Cmp(bound) < 0 {
				return nil, fail("budget_exceeded", 402)
			}
		}
	}
	deadline := r.clock().Add(time.Duration(in.Limits.TimeoutMs) * time.Millisecond)
	reservation, err := r.store.Reserve(ctx, p, key, digest, service, legs, deadline, decision)
	if err != nil {
		return nil, err
	}
	if !reservation.Fresh {
		return replayed(reservation.Request), nil
	}
	return r.execute(ctx, p, in, service, candidates, legs, reservation.Request, emit)
}

// execute runs the attempts of an admitted request. From here on every store call uses a context that
// survives the caller's cancellation, because a disconnected client must not leave funds held.
func (r *Router) execute(ctx context.Context, p contracts.Principal, in contracts.Invocation, service contracts.Service, candidates []routing.Candidate, legs []Leg, request *RequestRecord, emit func(contracts.StreamEvent) error) (result *Result, failure error) {
	bg := context.WithoutCancel(ctx)
	var (
		seq        int
		text       []byte
		started    bool
		finalUsage *contracts.Meters
		execution  = "failed"
		pending    bool
		lastModel  string
		toolBytes  int
		toolCalls  = map[int]*contracts.ToolCall{}
	)
	send := func(kind string, data map[string]any) error {
		seq++
		if emit == nil {
			return nil
		}
		return emit(contracts.StreamEvent{Type: kind, RequestID: request.ID, Sequence: seq, Data: data})
	}
	runCtx, abort := context.WithCancel(ctx)
	defer abort()
	timer := time.AfterFunc(time.Duration(in.Limits.TimeoutMs)*time.Millisecond, abort)
	defer timer.Stop()
	stopPolling := make(chan struct{})
	var polling sync.WaitGroup
	polling.Add(1)
	go func() {
		defer polling.Done()
		tick := time.NewTicker(100 * time.Millisecond)
		defer tick.Stop()
		for {
			select {
			case <-stopPolling:
				return
			case <-runCtx.Done():
				return
			case <-tick.C:
				rec, err := r.store.Get(bg, p, request.ID)
				if err != nil || rec.CancelRequested {
					abort()
					return
				}
			}
		}
	}()
	defer func() {
		close(stopPolling)
		polling.Wait()
	}()
	defer func() {
		if failure == nil {
			return
		}
		// An unexpected storage or transport failure must never turn an uncertain dispatch into a refund.
		outcome := "unknown"
		if started {
			outcome = "partial"
		}
		_ = r.store.Finish(bg, p, request.ID, outcome, nil, true)
		var re *contracts.RouterError
		if !errors.As(failure, &re) {
			re = contracts.NewError("execution_unknown", 503)
		}
		failure = &contracts.RouterErrorWithRequest{RouterError: re, RequestID: request.ID}
	}()

	if err := send("request.started", map[string]any{"service_version": service.Version}); err != nil {
		return nil, err
	}
	attempts := candidates
	if int64(len(attempts)) > service.MaxAttempts {
		attempts = attempts[:service.MaxAttempts]
	}
	for _, candidate := range attempts {
		if runCtx.Err() != nil {
			execution = "cancelled"
			break
		}
		provider := r.providers[candidate.Offering.Provider]
		if provider == nil {
			return nil, fail("provider_unavailable", 503)
		}
		attempt, err := r.store.Begin(bg, p, request.ID, candidate.Offering, candidate.Cost.String())
		if err != nil {
			var re *contracts.RouterError
			if errors.As(err, &re) && (re.Code == "budget_exceeded" || re.Code == "connection_disabled" || re.Code == "cancelled") {
				execution = "failed"
				if re.Code == "cancelled" {
					execution = "cancelled"
				}
				continue
			}
			return nil, err
		}
		var (
			usage      *contracts.Meters
			upstream   string
			done       bool
			completion = "succeeded"
		)
		lastModel = candidate.Offering.Model
		callErr := provider.Invoke(runCtx, in, candidate.Offering, func(ev contracts.ProviderEvent) error {
			switch ev.Type {
			case "delta":
				if !started {
					if err := r.store.Output(bg, p, request.ID); err != nil {
						return err
					}
					started = true
				}
				text = append(text, ev.Text...)
				if len(text)+toolBytes > 1_000_000 {
					return &contracts.ProviderError{Code: "output_too_large"}
				}
				return send("output.delta", map[string]any{"text": ev.Text})
			case "tool_call":
				known := toolCalls[ev.Index]
				if in.Input.Tools == nil || ev.Index < 0 || ev.Index > 1<<53-1 ||
					known == nil && (len(toolCalls) >= 64 || ev.ID == "" || ev.Name == "") {
					return &contracts.ProviderError{Code: "invalid_tool_call"}
				}
				// A tool call is output: after this point a failure is partial and never falls back to another offering.
				if !started {
					if err := r.store.Output(bg, p, request.ID); err != nil {
						return err
					}
					started = true
				}
				toolBytes += len(ev.ArgumentsDelta)
				if len(text)+toolBytes > 1_000_000 {
					return &contracts.ProviderError{Code: "output_too_large"}
				}
				data := map[string]any{"index": ev.Index, "arguments_delta": ev.ArgumentsDelta}
				if known != nil {
					known.Arguments += ev.ArgumentsDelta
				} else {
					toolCalls[ev.Index] = &contracts.ToolCall{ID: ev.ID, Name: ev.Name, Arguments: ev.ArgumentsDelta}
					data["id"], data["name"] = ev.ID, ev.Name
				}
				return send("output.tool_call.delta", data)
			case "usage":
				if !ev.Usage.Valid() {
					return &contracts.ProviderError{Code: "invalid_usage"}
				}
				m := ev.Usage
				usage = &m
				upstream = ev.UpstreamID
			default:
				done = true
				if ev.Completion != "" {
					completion = ev.Completion
				}
			}
			return nil
		})
		if callErr == nil {
			if !done || usage == nil {
				callErr = &contracts.ProviderError{Code: "usage_unknown"}
			} else if code := checkUsage(in, *usage, completion); code != "" {
				usage = nil
				callErr = &contracts.ProviderError{Code: code}
			}
		}
		if callErr == nil {
			cost, err := pricing.Charge(usage.Usage(), candidate.Offering.Price)
			if err == nil {
				costText := cost.String()
				err = r.store.AttemptResult(bg, p, request.ID, attempt, completion, usage, &costText, upstream)
			}
			if err == nil {
				finalUsage = usage
				execution = completion
				pending = cost.Cmp(candidate.Cost) > 0
				break
			}
			callErr = err
		}
		var pe *contracts.ProviderError
		definitive := errors.As(callErr, &pe) && pe.Definitive && !started
		status, cost := "unknown", (*string)(nil)
		switch {
		case usage != nil:
			if c, err := pricing.Charge(usage.Usage(), candidate.Offering.Price); err == nil {
				s := c.String()
				cost = &s
			}
		case definitive:
			zero := "0"
			cost = &zero
		}
		if definitive {
			status = "failed"
		}
		if err := r.store.AttemptResult(bg, p, request.ID, attempt, status, usage, cost, upstream); err != nil {
			return nil, err
		}
		if !definitive {
			pending = true
			execution = "unknown"
			if started {
				execution = "partial"
			}
			break
		}
		if !pe.Retryable {
			break
		}
	}
	if err := r.store.Finish(bg, p, request.ID, execution, finalUsage, pending); err != nil {
		return nil, err
	}
	settled, err := r.store.Get(bg, p, request.ID)
	if err != nil {
		return nil, err
	}
	// Only a finished response has whole tool calls; a cut-off one must never look callable.
	var calls []contracts.ToolCall
	if execution == "succeeded" && len(toolCalls) > 0 {
		indexes := make([]int, 0, len(toolCalls))
		for i := range toolCalls {
			indexes = append(indexes, i)
		}
		sort.Ints(indexes)
		for _, i := range indexes {
			call := *toolCalls[i]
			if call.Arguments = contracts.JSTrim(call.Arguments); call.Arguments == "" {
				call.Arguments = "{}"
			}
			calls = append(calls, call)
			if err := send("output.tool_call.completed", map[string]any{"index": i, "id": call.ID, "name": call.Name, "arguments": call.Arguments}); err != nil {
				return nil, err
			}
		}
	}
	finish := ""
	switch {
	case execution == "partial" && finalUsage != nil:
		finish = "length"
	case execution != "succeeded":
	case len(calls) > 0:
		finish = "tool_calls"
	default:
		finish = "stop"
	}
	res := r.result(p, service, legs, request, settled, lastModel, string(text), execution, finalUsage)
	res.Output.ToolCalls, res.FinishReason = calls, finish
	kind := "request.failed"
	if execution == "succeeded" {
		kind = "request.completed"
	}
	data := map[string]any{"execution_status": execution, "billing_status": settled.Billing, "usage": res.Usage}
	if finish != "" {
		data["finish_reason"] = finish
	}
	if err := send(kind, data); err != nil {
		return nil, err
	}
	return res, nil
}

// checkUsage applies the per-kind limits to what a provider reported. It returns an error code, or "".
func checkUsage(in contracts.Invocation, u contracts.Meters, completion string) string {
	if task := in.Input.Task; task != nil {
		meter := contracts.KindMeter[task.Kind]
		if u.Input != 0 || u.Cached != 0 || u.CacheWrite != 0 || u.Output != 0 {
			return "usage_out_of_bounds"
		}
		for _, k := range contracts.MediaKeys {
			if u.Media(k) > in.Limits.MaxUsage[k] {
				return "usage_out_of_bounds"
			}
		}
		if meter == "tasks" {
			want := int64(0)
			if completion == "succeeded" {
				want = 1
			}
			if u.Media("tasks") != want {
				return "usage_out_of_bounds"
			}
		}
		return ""
	}
	for _, k := range contracts.MediaKeys {
		if u.Media(k) != 0 {
			return "unexpected_meter"
		}
	}
	return ""
}

func (r *Router) result(p contracts.Principal, service contracts.Service, legs []Leg, request, settled *RequestRecord, model, text, execution string, usage *contracts.Meters) *Result {
	version := service.Version
	res := &Result{
		ID:              request.ID,
		ExecutionStatus: execution,
		BillingStatus:   settled.Billing,
		ServiceVersion:  &version,
		Model:           model,
		Tier:            service.Tier,
		Output:          &Output{Role: "assistant", Content: text},
		Billing:         &BillingReport{Charges: []Charge{}},
	}
	switch {
	case usage != nil:
		res.Usage = &UsageReport{Status: "confirmed", Meters: usage}
	case settled.Billing == "released":
		res.Usage = &UsageReport{Status: "not_billable"}
	default:
		res.Usage = &UsageReport{Status: "pending"}
	}
	if usage != nil && settled.Billing == "settled" {
		for _, l := range legs {
			if l.Kind != "customer_sale" && !p.HasScope("billing:read") {
				continue
			}
			if due, err := pricing.Charge(usage.Usage(), l.Price); err == nil {
				res.Billing.Charges = append(res.Billing.Charges, Charge{Kind: l.Kind, Asset: l.Price.Asset, Atoms: due.String(), Scale: 9})
			}
		}
	}
	return res
}
