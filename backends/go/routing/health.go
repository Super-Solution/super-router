// SPDX-License-Identifier: Apache-2.0

package routing

import (
	"sync"

	"github.com/Super-Solution/super-router/backends/go/contracts"
)

// HealthPolicy says how long each class of failure keeps an offering out, and how that grows.
type HealthPolicy struct {
	RateLimitedMs         int64
	QuotaExhaustedMs      int64
	CredentialsRejectedMs int64
	ModelUnavailableMs    int64
	RegionBlockedMs       int64
	// MaxMs is the longest a cooldown can grow to, however many failures in a row.
	MaxMs int64
	// ProbeLeaseMs is how long a probe may stay unanswered before another caller is admitted in its place.
	ProbeLeaseMs int64
}

// DefaultHealthPolicy is the policy a zero HealthBook uses.
func DefaultHealthPolicy() HealthPolicy {
	return HealthPolicy{
		RateLimitedMs: 5_000, QuotaExhaustedMs: 300_000, CredentialsRejectedMs: 300_000,
		ModelUnavailableMs: 600_000, RegionBlockedMs: 600_000, MaxMs: 1_800_000, ProbeLeaseMs: 30_000,
	}
}

func (p HealthPolicy) base(c contracts.FailureClass) int64 {
	switch c {
	case contracts.FailureRateLimited:
		return p.RateLimitedMs
	case contracts.FailureQuotaExhausted:
		return p.QuotaExhaustedMs
	case contracts.FailureCredentialsRejected:
		return p.CredentialsRejectedMs
	case contracts.FailureModelUnavailable:
		return p.ModelUnavailableMs
	case contracts.FailureRegionBlocked:
		return p.RegionBlockedMs
	}
	return 0
}

// AdmitKind is what a caller is told about an offering.
type AdmitKind int

// The three answers.
const (
	// AdmitOpen: nothing is known against the offering.
	AdmitOpen AdmitKind = iota
	// AdmitProbe: its cooldown has ended and this caller is the one to try it. Report the outcome.
	AdmitProbe
	// AdmitCooling: skip it until Admit.UntilMs (the end of the cooldown, or of the running probe's lease).
	AdmitCooling
)

// Admit is the answer of HealthBook.Admit.
type Admit struct {
	Kind    AdmitKind
	UntilMs int64
	Class   contracts.FailureClass
}

type healthRecord struct {
	class      contracts.FailureClass
	streak     uint
	untilMs    int64
	probeUntil *int64
}

// HealthBook remembers which offerings to leave alone for a while.
//
// When an offering's own failure repeats (its key is rejected, its account is empty, its model is gone, it is rate
// limited), every request that reaches it first pays one wasted attempt before the router falls through. The book
// remembers the failure and lets routing skip that offering until it is worth trying again. It is a pure state
// machine: no clock, no goroutine, no I/O; every call is given nowMs, so a test replays a whole incident at once and
// the Rust and Go copies are checked against the same sequences. It is safe for concurrent use (the Rust copy takes
// &mut self and a host shares it behind a lock).
//
// Rules, each pinned by a conformance case: only an offering-scoped class is recorded (a refused request says nothing
// about the offering, a server error leaves the outcome unknown); a failure starts a cooldown whose base depends on
// the class and doubles with each consecutive failure up to MaxMs; a failure reported while already cooling is
// ignored (it belongs to a request in flight before the cooldown began); when a cooldown ends the first caller is
// admitted as a probe and everyone else keeps skipping the offering until the probe reports or its lease runs out; one
// success clears the record, one failure starts the next, longer, cooldown.
type HealthBook struct {
	mu      sync.Mutex
	policy  HealthPolicy
	records map[string]*healthRecord
}

// NewHealthBook returns an empty book.
func NewHealthBook(policy HealthPolicy) *HealthBook {
	return &HealthBook{policy: policy, records: map[string]*healthRecord{}}
}

// Admit says whether key may be tried now. AdmitProbe claims the single probe for its lease.
func (b *HealthBook) Admit(key string, nowMs int64) Admit {
	b.mu.Lock()
	defer b.mu.Unlock()
	r, ok := b.records[key]
	if !ok {
		return Admit{Kind: AdmitOpen}
	}
	if nowMs < r.untilMs {
		return Admit{Kind: AdmitCooling, UntilMs: r.untilMs, Class: r.class}
	}
	if r.probeUntil != nil && nowMs < *r.probeUntil {
		return Admit{Kind: AdmitCooling, UntilMs: *r.probeUntil, Class: r.class}
	}
	until := addSat(nowMs, b.policy.ProbeLeaseMs)
	r.probeUntil = &until
	return Admit{Kind: AdmitProbe}
}

// Failed records that an attempt on key failed with class. Classes that do not belong to the offering are ignored.
func (b *HealthBook) Failed(key string, class contracts.FailureClass, nowMs int64) {
	b.mu.Lock()
	defer b.mu.Unlock()
	if !class.IsOfferingScoped() {
		return
	}
	base := max(b.policy.base(class), 0)
	streak := uint(1)
	if r, ok := b.records[key]; ok {
		if nowMs < r.untilMs {
			return
		}
		streak = r.streak + 1
	}
	// base * 2^(streak-1), saturating, never above the ceiling.
	shift := min(streak-1, 32)
	var grown int64
	if base > (1<<62)>>shift {
		grown = 1 << 62
	} else {
		grown = base << shift
	}
	length := min(grown, max(b.policy.MaxMs, base))
	b.records[key] = &healthRecord{class: class, streak: streak, untilMs: addSat(nowMs, length)}
}

// Succeeded records that an attempt on key succeeded: whatever was held against it is cleared.
func (b *HealthBook) Succeeded(key string) {
	b.mu.Lock()
	defer b.mu.Unlock()
	delete(b.records, key)
}

// Len is how many offerings currently have something held against them.
func (b *HealthBook) Len() int {
	b.mu.Lock()
	defer b.mu.Unlock()
	return len(b.records)
}

func addSat(a, b int64) int64 {
	if b > 0 && a > (1<<63-1)-b {
		return 1<<63 - 1
	}
	return a + b
}
