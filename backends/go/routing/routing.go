// SPDX-License-Identifier: Apache-2.0

// Package routing ports the offering selection of the TypeScript reference (`route` in
// packages/core): hard limits first, then weighted scoring, ordered by score and then by id.
// It reproduces every case in conformance/routing.v1.json, including the exact IEEE-754
// scores. Offering ids must sort the same byte-wise as in TypeScript (use [a-z0-9]).
package routing

import (
	"math/big"
	"sort"
	"time"

	"github.com/Super-Solution/super-router/backends/go/contracts"
	"github.com/Super-Solution/super-router/backends/go/pricing"
)

// MediaKeys are the media meters, in the order the TypeScript reference iterates them.
var MediaKeys = contracts.MediaKeys

var kindMeter = contracts.KindMeter

// Offering, Weights, Service and Invocation are the shared wire types of package contracts.
type (
	Offering   = contracts.Offering
	Weights    = contracts.Weights
	Service    = contracts.Service
	Invocation = contracts.Invocation
)

// Candidate is an offering that passed every hard limit.
type Candidate struct {
	Offering Offering
	Cost     *big.Int
	Score    float64
	// Downgrade marks an offering of the service's downgrade policy: a lower tier, tried only after the primary ones.
	Downgrade bool
}

// Excluded names an offering that was dropped and the first reason that applied.
type Excluded struct {
	ID     string `json:"id"`
	Reason string `json:"reason"`
}

func sameKind(a, b *string) bool {
	if a == nil || b == nil {
		return a == nil && b == nil
	}
	return *a == *b
}

func contains(list []string, x string) bool {
	for _, v := range list {
		if v == x {
			return true
		}
	}
	return false
}

func atoms(s string) *big.Int {
	n, ok := new(big.Int).SetString(s, 10)
	if !ok {
		panic("routing: not a decimal integer: " + s)
	}
	return n
}

func invocationBound(in Invocation, maxInput int64, price pricing.Price) (*big.Int, error) {
	if in.Input.Task == nil {
		return pricing.Bound(maxInput, in.Limits.MaxOutputTokens, price), nil
	}
	usage := pricing.Usage{Media: map[string]int64{}, Units: map[string]int64{}}
	for k, n := range in.Limits.MaxUsage {
		if pricing.ValidUnitName(k) {
			usage.Units[k] = n
		} else {
			usage.Media[k] = n
		}
	}
	return pricing.Charge(usage, price)
}

// InvocationBound is the most a single attempt can cost on one price: the token bound for chat, or the
// priced media limits for a task. Routing and the router both use it, so the reserved amount and the
// candidate cost can never disagree.
func InvocationBound(in Invocation, maxInput int64, price pricing.Price) (*big.Int, error) {
	return invocationBound(in, maxInput, price)
}

func minOne(x float64) float64 {
	if x < 1 {
		return x
	}
	return 1
}

// labelReason applies label evidence: an expired label always excludes, and an unverified (or undocumented)
// one does when the service requires verified labels.
func labelReason(s Service, o Offering, nowMs int64) string {
	for _, label := range s.RequiredLabels {
		if e := o.Evidence(label); e != nil && e.ExpiresAt != nil {
			if expiry, ok := contracts.ParseUTCMillis(*e.ExpiresAt); ok && expiry <= nowMs {
				return "label_expired"
			}
		}
	}
	if s.RequireVerifiedLabels != nil && *s.RequireVerifiedLabels {
		for _, label := range s.RequiredLabels {
			if e := o.Evidence(label); e == nil || e.Verification != "verified" {
				return "label_unverified"
			}
		}
	}
	return ""
}

// Route applies the hard limits and then scores what is left, using the wall clock for label expiry. A pricing
// error from a task bound (an unpriced meter) aborts the whole call, as in the reference.
func Route(service Service, in Invocation) ([]Candidate, []Excluded, error) {
	return RouteAt(service, in, time.Now().UnixMilli())
}

// RouteAt is Route at an injected time, in milliseconds since the epoch. Downgrade offerings face the same hard
// limits and always sort after every primary offering.
func RouteAt(service Service, in Invocation, nowMs int64) ([]Candidate, []Excluded, error) {
	var candidates []Candidate
	excluded := []Excluded{}
	var taskKind *string
	if in.Input.Task != nil {
		taskKind = &in.Input.Task.Kind
	}
	maxProviderCost := atoms(service.MaxProviderCost)
	costScale := atoms(service.CostScale)
	lower := map[string]bool{}
	if d := service.DowngradePolicy; d != nil && service.Mode == "tier" {
		for _, o := range d.Offerings {
			lower[o.ID] = true
		}
	}
	for _, o := range service.AllOfferings() {
		reason := ""
		switch {
		case !sameKind(o.TaskKind, taskKind) || !sameKind(service.TaskKind, taskKind):
			reason = "task_kind_mismatch"
		case !o.Enabled:
			reason = "disabled"
		case service.Mode == "specific" && o.Model != service.Model:
			reason = "model_mismatch"
		case !contains(service.Regions, o.Region):
			reason = "region_mismatch"
		case missingLabel(service.RequiredLabels, o.Labels):
			reason = "label_mismatch"
		default:
			reason = labelReason(service, o, nowMs)
		}
		switch {
		case reason != "":
		case in.Limits.MaxOutputTokens > o.MaxOutputTokens:
			reason = "output_limit"
		case in.ResponseFormat != nil && !o.HasCapability("structured_output"):
			reason = "structured_output_unsupported"
		}
		if reason == "" && in.Input.Task != nil {
			key := in.Input.Task.MeterName()
			units := in.Limits.MaxUsage[key]
			_, pricedMedia := o.Price.Media[key]
			_, pricedUnit := o.Price.Units[key]
			if units == 0 || units > o.MaxUsage[key] || !pricedMedia && !pricedUnit {
				reason = "meter_limit"
			}
		}
		cost := new(big.Int)
		if reason == "" {
			var err error
			if cost, err = invocationBound(in, o.MaxInputTokens, o.Price); err != nil {
				return nil, nil, err
			}
			if cost.Cmp(maxProviderCost) > 0 {
				reason = "budget_exceeded"
			}
		}
		if reason != "" {
			excluded = append(excluded, Excluded{ID: o.ID, Reason: reason})
			continue
		}
		scaled := new(big.Int).Mul(cost, big.NewInt(1_000_000))
		scaled.Quo(scaled, costScale)
		whole, _ := new(big.Float).SetInt(scaled).Float64()
		ratio := whole / 1_000_000
		unknown := service.UnknownScore
		latency := unknown
		if o.LatencyMs != nil {
			latency = 1 - minOne(*o.LatencyMs/service.LatencyScaleMs)
		}
		reliability, quality := unknown, unknown
		if o.Reliability != nil {
			reliability = *o.Reliability
		}
		if o.Quality != nil {
			quality = *o.Quality
		}
		// The explicit conversions stop the compiler fusing a multiply into the next add,
		// which would change the last bit of the score on some architectures.
		w := service.Weights
		score := float64(w.Cost*(1-minOne(ratio))) + float64(w.Latency*latency)
		score += float64(w.Reliability * reliability)
		score += float64(w.Quality * quality)
		candidates = append(candidates, Candidate{Offering: o, Cost: cost, Score: score, Downgrade: lower[o.ID]})
	}
	sort.SliceStable(candidates, func(i, j int) bool {
		if candidates[i].Downgrade != candidates[j].Downgrade {
			return !candidates[i].Downgrade
		}
		if candidates[i].Score != candidates[j].Score {
			return candidates[i].Score > candidates[j].Score
		}
		return candidates[i].Offering.ID < candidates[j].Offering.ID
	})
	return candidates, excluded, nil
}

func missingLabel(required, have []string) bool {
	for _, r := range required {
		if !contains(have, r) {
			return true
		}
	}
	return false
}
