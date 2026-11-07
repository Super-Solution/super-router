// SPDX-License-Identifier: Apache-2.0

// Package routing ports the offering selection of the TypeScript reference (`route` in
// packages/core): hard limits first, then weighted scoring, ordered by score and then by id.
// It reproduces every case in conformance/routing.v1.json, including the exact IEEE-754
// scores. Offering ids must sort the same byte-wise as in TypeScript (use [a-z0-9]).
package routing

import (
	"math/big"
	"sort"

	"github.com/Super-Solution/super-router/backends/go/pricing"
)

// MediaKeys are the media meters, in the order the TypeScript reference iterates them.
var MediaKeys = []string{"images", "video_ms", "audio_ms", "tasks"}

var kindMeter = map[string]string{"image": "images", "video": "video_ms", "audio": "audio_ms", "task": "tasks"}

// Offering is one concrete model route a service can use.
type Offering struct {
	ID              string           `json:"id"`
	TaskKind        *string          `json:"task_kind"`
	MaxUsage        map[string]int64 `json:"max_usage"`
	Model           string           `json:"model"`
	Region          string           `json:"region"`
	Labels          []string         `json:"labels"`
	Enabled         bool             `json:"enabled"`
	MaxInputTokens  int64            `json:"max_input_tokens"`
	MaxOutputTokens int64            `json:"max_output_tokens"`
	Price           pricing.Price    `json:"price"`
	LatencyMs       *float64         `json:"latency_ms"`
	Reliability     *float64         `json:"reliability"`
	Quality         *float64         `json:"quality"`
}

// Weights are the scoring weights; they sum to one.
type Weights struct {
	Cost        float64 `json:"cost"`
	Latency     float64 `json:"latency"`
	Reliability float64 `json:"reliability"`
	Quality     float64 `json:"quality"`
}

// Service is the part of a service definition that routing reads.
type Service struct {
	TaskKind        *string    `json:"task_kind"`
	Mode            string     `json:"mode"`
	Model           string     `json:"model"`
	Offerings       []Offering `json:"offerings"`
	Regions         []string   `json:"regions"`
	RequiredLabels  []string   `json:"required_labels"`
	MaxProviderCost string     `json:"max_provider_cost"`
	Weights         Weights    `json:"weights"`
	CostScale       string     `json:"cost_scale"`
	LatencyScaleMs  float64    `json:"latency_scale_ms"`
	UnknownScore    float64    `json:"unknown_score"`
}

// Invocation is the part of a request that routing reads.
type Invocation struct {
	Input struct {
		Task *struct {
			Kind string `json:"kind"`
		} `json:"task"`
	} `json:"input"`
	Limits struct {
		MaxOutputTokens int64            `json:"max_output_tokens"`
		MaxUsage        map[string]int64 `json:"max_usage"`
	} `json:"limits"`
}

// Candidate is an offering that passed every hard limit.
type Candidate struct {
	Offering Offering
	Cost     *big.Int
	Score    float64
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
	usage := pricing.Usage{Media: map[string]int64{}}
	for _, k := range MediaKeys {
		usage.Media[k] = in.Limits.MaxUsage[k]
	}
	return pricing.Charge(usage, price)
}

func minOne(x float64) float64 {
	if x < 1 {
		return x
	}
	return 1
}

// Route applies the hard limits and then scores what is left. A pricing error from a task
// bound (an unpriced meter) aborts the whole call, as in the reference.
func Route(service Service, in Invocation) ([]Candidate, []Excluded, error) {
	var candidates []Candidate
	excluded := []Excluded{}
	var taskKind *string
	if in.Input.Task != nil {
		taskKind = &in.Input.Task.Kind
	}
	maxProviderCost := atoms(service.MaxProviderCost)
	costScale := atoms(service.CostScale)
	for _, o := range service.Offerings {
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
		case in.Limits.MaxOutputTokens > o.MaxOutputTokens:
			reason = "output_limit"
		}
		if reason == "" && in.Input.Task != nil {
			key := kindMeter[in.Input.Task.Kind]
			units := in.Limits.MaxUsage[key]
			_, priced := o.Price.Media[key]
			if units == 0 || units > o.MaxUsage[key] || !priced {
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
		candidates = append(candidates, Candidate{Offering: o, Cost: cost, Score: score})
	}
	sort.SliceStable(candidates, func(i, j int) bool {
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
