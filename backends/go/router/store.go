// SPDX-License-Identifier: Apache-2.0

package router

import (
	"context"
	"time"

	"github.com/Super-Solution/super-router/backends/go/contracts"
	"github.com/Super-Solution/super-router/backends/go/pricing"
	"github.com/Super-Solution/super-router/backends/go/routing"
)

// Leg is one account the request charges: the customer's sale account and, optionally, the platform's.
type Leg struct {
	Kind     string        `json:"kind"` // "customer_sale" or "platform_sale"
	Account  string        `json:"account"`
	Price    pricing.Price `json:"price"`
	Reserved string        `json:"reserved"` // atoms held before the call
}

// RequestRecord is what a store knows about one request. Treat it as read-only.
type RequestRecord struct {
	ID              string
	Workspace       string
	Environment     string
	Customer        string
	Service         contracts.Service
	Execution       string // accepted, running, succeeded, partial, unknown, failed, cancelled
	Billing         string // reserved, pending, settled, released
	CancelRequested bool
	OutputStarted   bool
	Deadline        time.Time
	Legs            []Leg
	ProviderSpent   string
}

// Reservation is the outcome of Store.Reserve. Fresh is false when the idempotency key was already admitted.
type Reservation struct {
	Request *RequestRecord
	Fresh   bool
}

// Decision records why the router chose its candidates; stores may persist it for audit.
type Decision struct {
	Excluded   []routing.Excluded  `json:"excluded"`
	Candidates []DecisionCandidate `json:"candidates"`
}

// DecisionCandidate is one routable offering with its bound cost and score.
type DecisionCandidate struct {
	Downgrade       *Downgrade                          `json:"downgrade,omitempty"`
	QualityMetadata *contracts.QualityMetadata          `json:"quality_metadata,omitempty"`
	LabelEvidence   *map[string]contracts.LabelEvidence `json:"label_evidence,omitempty"`
	ID              string                              `json:"id"`
	Score           float64                             `json:"score"`
	Cost            string                              `json:"cost"`
}

// Store is the storage port of the router. Every method must be atomic and safe for concurrent use.
// Implement it (and DataStore) to plug in another database; run the dataplane conformance scenarios against it.
type Store interface {
	// Replay returns the request admitted under key, nil when there is none, or idempotency_conflict (409)
	// when the key was used for a different request or customer.
	Replay(ctx context.Context, p contracts.Principal, key, digest string) (*RequestRecord, error)
	GetService(ctx context.Context, p contracts.Principal, id string, version *int64) (contracts.Service, error)
	// Accounts returns the legs a request for this service charges, or insufficient_balance (402) when an account is missing.
	Accounts(ctx context.Context, p contracts.Principal, s contracts.Service) ([]Leg, error)
	// Reserve admits a request and holds each leg's Reserved amount, all or nothing.
	Reserve(ctx context.Context, p contracts.Principal, key, digest string, s contracts.Service, legs []Leg, deadline time.Time, decision Decision) (Reservation, error)
	Get(ctx context.Context, p contracts.Principal, id string) (*RequestRecord, error)
	// Begin records a provider dispatch and returns its attempt id.
	Begin(ctx context.Context, p contracts.Principal, id string, o contracts.Offering, bound string) (string, error)
	// Output marks that the caller has seen output, after which a failure can no longer be refunded.
	Output(ctx context.Context, p contracts.Principal, id string) error
	AttemptResult(ctx context.Context, p contracts.Principal, id, attempt, status string, usage *contracts.Meters, cost *string, upstream string) error
	// Finish settles or releases the reservation, or leaves it pending when the outcome is uncertain.
	Finish(ctx context.Context, p contracts.Principal, id, execution string, usage *contracts.Meters, pending bool) error
	Cancel(ctx context.Context, p contracts.Principal, id string) error
}

// BalanceRow is one account as the customer sees it.
type BalanceRow struct {
	ID        string `json:"id"`
	Kind      string `json:"kind"`
	Asset     string `json:"asset"`
	Balance   string `json:"balance"`
	Held      string `json:"held"`
	Available string `json:"available"`
}

// DataStore is everything the data-plane server and worker need from storage beyond the router's own Store.
type DataStore interface {
	Store
	// Publish stores a service version. mode is "cloud" or "self-host"; expectedVersion, when set, must be the current version.
	Publish(ctx context.Context, p contracts.Principal, s contracts.Service, mode string, expectedVersion *int64) (contracts.Service, error)
	List(ctx context.Context, p contracts.Principal) ([]contracts.Service, error)
	Balance(ctx context.Context, p contracts.Principal) ([]BalanceRow, error)
	// Grant adds a funding bucket. source is its idempotency key.
	Grant(ctx context.Context, p contracts.Principal, kind, asset, amount, source string, expires *time.Time) (string, error)
	Disable(ctx context.Context, p contracts.Principal, offering string) error
	// Recover settles or releases requests whose deadline passed, never redispatching an uncertain attempt.
	// It returns how many requests it processed.
	Recover(ctx context.Context) (int, error)
}

// Clock returns the current time. Stores and the router accept one so tests control time.
type Clock func() time.Time
