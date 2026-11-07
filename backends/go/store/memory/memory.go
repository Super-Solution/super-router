// SPDX-License-Identifier: Apache-2.0

// Package memory is the reference in-memory implementation of router.DataStore.
//
// It follows the PostgreSQL store's semantics exactly: idempotent admission, bucketed holds, a provider cost
// budget, a balanced journal and an append-only outbox. One mutex makes every method atomic. State is lost on
// restart, which makes it right for tests, demos and single-process self-hosting without a database.
package memory

import (
	"context"
	"encoding/json"
	"math"
	"math/big"
	"sort"
	"strconv"
	"sync"
	"time"

	"github.com/Super-Solution/super-router/backends/go/contracts"
	"github.com/Super-Solution/super-router/backends/go/internal/uuid"
	"github.com/Super-Solution/super-router/backends/go/pricing"
	"github.com/Super-Solution/super-router/backends/go/router"
)

type account struct {
	id, owner, kind, asset string
	balance, held          *big.Int
}

type bucket struct {
	id, accountID, source, sourceKind string
	seq                               int
	expiresAt                         *int64 // epoch ms
	remaining, granted, held          *big.Int
	serviceIDs                        []string // nil means every service
}

type allocation struct {
	bucket  string
	account string
	amount  *big.Int
	charged *big.Int
}

type attempt struct {
	id, offering, status, upstream string
	usage                          *contracts.Meters
	cost                           *big.Int
	bound                          *big.Int
}

type request struct {
	record        router.RequestRecord
	digest        string
	providerSpent *big.Int
	providerHeld  *big.Int
	attempts      []*attempt
	allocations   []*allocation
}

// JournalEntry is one balanced double-entry line.
type JournalEntry struct {
	Key, Asset, Debit, Credit, Amount, Reason string
}

// OutboxEvent is a domain event waiting for delivery.
type OutboxEvent struct {
	ID      string
	Type    string // billing.settled, billing.released or reconciliation.required
	Request string
}

// BucketView is a funding bucket as reported by Inspect.
type BucketView struct {
	Source    string `json:"source"`
	Remaining string `json:"remaining"`
	Held      string `json:"held"`
}

// Inspection is a read-only snapshot for tests and diagnostics.
type Inspection struct {
	Journal        []JournalEntry
	Outbox         []OutboxEvent
	Reconciliation map[string]string
	Buckets        []BucketView
}

type tenant struct {
	services       map[string][]contracts.Service
	accounts       map[string]*account
	accountsByID   map[string]*account
	buckets        []*bucket
	requests       map[string]*request
	idem           map[string]string
	disabled       map[string]bool
	journal        []JournalEntry
	outbox         []OutboxEvent
	reconciliation map[string]string
	active         map[string]int64
	entitlements   []entitlement
	workspace, env string
}

type entitlement struct {
	customer   string
	serviceIDs []string
	start, end int64
}

func newTenant(p contracts.Principal) *tenant {
	return &tenant{
		services: map[string][]contracts.Service{}, accounts: map[string]*account{}, accountsByID: map[string]*account{},
		requests: map[string]*request{}, idem: map[string]string{}, disabled: map[string]bool{},
		reconciliation: map[string]string{}, active: map[string]int64{}, workspace: p.Workspace, env: p.Environment,
	}
}

// Store implements router.DataStore in memory.
type Store struct {
	mu      sync.Mutex
	tenants map[string]*tenant
	seq     int
	clock   router.Clock
}

var _ router.DataStore = (*Store)(nil)

// New creates a store. A nil clock means time.Now.
func New(clock router.Clock) *Store {
	if clock == nil {
		clock = time.Now
	}
	return &Store{tenants: map[string]*tenant{}, clock: clock}
}

func (s *Store) tenant(p contracts.Principal) *tenant {
	key := p.Workspace + "\x00" + p.Environment
	t := s.tenants[key]
	if t == nil {
		t = newTenant(p)
		s.tenants[key] = t
	}
	return t
}

func (s *Store) id(prefix string) string {
	s.seq++
	return prefix + pad8(s.seq)
}

func pad8(n int) string {
	t := strconv.Itoa(n)
	for len(t) < 8 {
		t = "0" + t
	}
	return t
}

func (s *Store) now() int64 { return s.clock().UnixMilli() }

func fail(code string, status int) error { return contracts.NewError(code, status) }

func (t *tenant) row(p contracts.Principal, id string) (*request, error) {
	r := t.requests[id]
	if r == nil || r.record.Customer != p.Customer {
		return nil, fail("resource_not_found", 404)
	}
	if !p.MayUse(r.record.Service.ID) {
		return nil, fail("scope_denied", 403)
	}
	return r, nil
}

// view is what callers see: a copy, so nothing can mutate store state.
func view(r *request) *router.RequestRecord {
	rec := r.record
	rec.Legs = append([]router.Leg(nil), r.record.Legs...)
	rec.ProviderSpent = r.providerSpent.String()
	return &rec
}

func same(a, b any) bool {
	x, _ := json.Marshal(a)
	y, _ := json.Marshal(b)
	return string(x) == string(y)
}

func clone(s contracts.Service) contracts.Service {
	raw, _ := json.Marshal(s)
	var out contracts.Service
	_ = json.Unmarshal(raw, &out)
	if out.RequiredLabels == nil {
		out.RequiredLabels = []string{}
	}
	return out
}

func rank(kind string) int {
	switch kind {
	case "subscription":
		return 0
	case "promotion":
		return 1
	}
	return 2
}

func expiry(b *bucket) int64 {
	if b.expiresAt == nil {
		return math.MaxInt64
	}
	return *b.expiresAt
}

func byExpiry(a, b *bucket) bool {
	if expiry(a) != expiry(b) {
		return expiry(a) < expiry(b)
	}
	if rank(a.sourceKind) != rank(b.sourceKind) {
		return rank(a.sourceKind) < rank(b.sourceKind)
	}
	return a.seq < b.seq
}

func atoms(s string) *big.Int {
	n, ok := new(big.Int).SetString(s, 10)
	if !ok {
		panic("memory: not a decimal integer: " + s)
	}
	return n
}

func contains(list []string, x string) bool {
	for _, v := range list {
		if v == x {
			return true
		}
	}
	return false
}

// Replay implements router.Store.
func (s *Store) Replay(_ context.Context, p contracts.Principal, key, digest string) (*router.RequestRecord, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	t := s.tenant(p)
	id, ok := t.idem[key]
	if !ok {
		return nil, nil
	}
	r := t.requests[id]
	if r.digest != digest || r.record.Customer != p.Customer {
		return nil, fail("idempotency_conflict", 409)
	}
	return view(r), nil
}

// Get implements router.Store.
func (s *Store) Get(_ context.Context, p contracts.Principal, id string) (*router.RequestRecord, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	r, err := s.tenant(p).row(p, id)
	if err != nil {
		return nil, err
	}
	return view(r), nil
}

// Publish implements router.DataStore.
func (s *Store) Publish(_ context.Context, p contracts.Principal, svc contracts.Service, mode string, expectedVersion *int64) (contracts.Service, error) {
	if !p.HasScope("service:write") || !p.MayUse(svc.ID) {
		return contracts.Service{}, fail("scope_denied", 403)
	}
	if mode == "" {
		mode = "self-host"
	}
	if mode == "cloud" {
		for _, o := range svc.Offerings {
			if o.Source == "byok" {
				return contracts.Service{}, fail("byok_not_allowed", 403)
			}
		}
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	t := s.tenant(p)
	versions := t.services[svc.ID]
	var previous *contracts.Service
	if len(versions) > 0 {
		previous = &versions[len(versions)-1]
	}
	if expectedVersion != nil && (previous == nil || previous.Version != *expectedVersion) {
		return contracts.Service{}, fail("service_version_conflict", 409)
	}
	if mode == "cloud" && p.AuthKeyID != "" {
		if previous == nil || !same(previous.PlatformPrice, svc.PlatformPrice) || !same(previous.Offerings, svc.Offerings) {
			return contracts.Service{}, fail("platform_pricing_protected", 403)
		}
	}
	for _, v := range versions {
		if v.Version == svc.Version {
			if !same(v, svc) {
				return contracts.Service{}, fail("version_exists", 409)
			}
			return svc, nil
		}
	}
	if previous != nil && svc.Version <= previous.Version {
		return contracts.Service{}, fail("service_version_conflict", 409)
	}
	t.services[svc.ID] = append(versions, clone(svc))
	t.active[svc.ID] = svc.Version
	return svc, nil
}

// GetService implements router.Store.
func (s *Store) GetService(_ context.Context, p contracts.Principal, id string, version *int64) (contracts.Service, error) {
	if !p.MayUse(id) {
		return contracts.Service{}, fail("scope_denied", 403)
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	t := s.tenant(p)
	wanted, ok := t.active[id]
	if version != nil {
		wanted, ok = *version, true
	}
	if ok {
		for _, v := range t.services[id] {
			if v.Version == wanted {
				return clone(v), nil
			}
		}
	}
	return contracts.Service{}, fail("resource_not_found", 404)
}

// List implements router.DataStore.
func (s *Store) List(_ context.Context, p contracts.Principal) ([]contracts.Service, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	t := s.tenant(p)
	ids := make([]string, 0, len(t.active))
	for id := range t.active {
		ids = append(ids, id)
	}
	sort.Strings(ids)
	if len(ids) > 100 {
		ids = ids[:100]
	}
	out := []contracts.Service{}
	for _, id := range ids {
		if !p.MayUse(id) {
			continue
		}
		for _, v := range t.services[id] {
			if v.Version == t.active[id] {
				out = append(out, clone(v))
			}
		}
	}
	return out, nil
}

// GrantOptions are the optional parts of a grant, used by hosts that sell subscriptions or promotions.
type GrantOptions struct {
	Expires    *time.Time
	ServiceIDs []string // nil means every service
	SourceKind string   // purchased (default), promotion or subscription
}

// Grant implements router.DataStore.
func (s *Store) Grant(ctx context.Context, p contracts.Principal, kind, asset, amount, source string, expires *time.Time) (string, error) {
	return s.GrantWith(ctx, p, kind, asset, amount, source, GrantOptions{Expires: expires})
}

// GrantWith is Grant with the full set of options.
func (s *Store) GrantWith(_ context.Context, p contracts.Principal, kind, asset, amount, source string, opt GrantOptions) (string, error) {
	if !p.HasScope("billing:write") {
		return "", fail("scope_denied", 403)
	}
	if !validAmount(amount) {
		return "", fail("invalid_amount", 400)
	}
	if opt.SourceKind == "" {
		opt.SourceKind = "purchased"
	}
	var expiresAt *int64
	if opt.Expires != nil {
		ms := opt.Expires.UnixMilli()
		expiresAt = &ms
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	t := s.tenant(p)
	owner := p.Customer
	if kind == "platform_sale" {
		owner = p.Workspace
	}
	for _, b := range t.buckets {
		if b.source != source {
			continue
		}
		a := t.accountsByID[b.accountID]
		if b.granted.String() != amount || a.asset != asset || a.owner != owner || a.kind != kind ||
			!sameInt(b.expiresAt, expiresAt) || !same(b.serviceIDs, opt.ServiceIDs) || b.sourceKind != opt.SourceKind {
			return "", fail("idempotency_conflict", 409)
		}
		return a.id, nil
	}
	key := owner + "\x00" + kind + "\x00" + asset
	a := t.accounts[key]
	if a == nil {
		a = &account{id: s.id("acct_"), owner: owner, kind: kind, asset: asset, balance: new(big.Int), held: new(big.Int)}
		t.accounts[key] = a
	}
	t.accountsByID[a.id] = a
	n := atoms(amount)
	a.balance.Add(a.balance, n)
	s.seq++
	t.buckets = append(t.buckets, &bucket{
		id: s.id("bkt_"), seq: s.seq, accountID: a.id, source: source, expiresAt: expiresAt, remaining: new(big.Int).Set(n),
		granted: new(big.Int).Set(n), held: new(big.Int), serviceIDs: opt.ServiceIDs, sourceKind: opt.SourceKind,
	})
	t.journal = append(t.journal, JournalEntry{Key: "grant:" + source, Asset: asset, Debit: "issuer:" + asset, Credit: a.id, Amount: amount, Reason: "grant"})
	return a.id, nil
}

func sameInt(a, b *int64) bool {
	if a == nil || b == nil {
		return a == nil && b == nil
	}
	return *a == *b
}

func validAmount(s string) bool {
	if len(s) < 1 || len(s) > 30 || s[0] == '0' {
		return false
	}
	for _, c := range s {
		if c < '0' || c > '9' {
			return false
		}
	}
	return true
}

// Accounts implements router.Store.
func (s *Store) Accounts(_ context.Context, p contracts.Principal, svc contracts.Service) ([]router.Leg, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	t := s.tenant(p)
	legs := []router.Leg{}
	for _, entry := range []struct {
		kind  string
		price *pricing.Price
	}{{"customer_sale", &svc.Price}, {"platform_sale", svc.PlatformPrice}} {
		if entry.price == nil {
			continue
		}
		owner := p.Customer
		if entry.kind == "platform_sale" {
			owner = p.Workspace
		}
		a := t.accounts[owner+"\x00"+entry.kind+"\x00"+entry.price.Asset]
		if a == nil {
			return nil, fail("insufficient_balance", 402)
		}
		legs = append(legs, router.Leg{Kind: entry.kind, Account: a.id, Price: *entry.price, Reserved: "0"})
	}
	return legs, nil
}

// Reserve implements router.Store.
func (s *Store) Reserve(_ context.Context, p contracts.Principal, key, digest string, svc contracts.Service, legs []router.Leg, deadline time.Time, _ router.Decision) (router.Reservation, error) {
	if !p.MayUse(svc.ID) {
		return router.Reservation{}, fail("scope_denied", 403)
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	t := s.tenant(p)
	now := s.now()
	if svc.RequiresSubscription != nil && *svc.RequiresSubscription {
		ok := false
		for _, e := range t.entitlements {
			if e.customer == p.Customer && contains(e.serviceIDs, svc.ID) && e.start <= now && now < e.end {
				ok = true
			}
		}
		if !ok {
			return router.Reservation{}, fail("subscription_required", 403)
		}
	}
	if oldID, ok := t.idem[key]; ok {
		old := t.requests[oldID]
		if old.digest != digest || old.record.Customer != p.Customer {
			return router.Reservation{}, fail("idempotency_conflict", 409)
		}
		return router.Reservation{Request: view(old), Fresh: false}, nil
	}
	if v, ok := t.active[svc.ID]; !ok || v != svc.Version {
		return router.Reservation{}, fail("inactive_service_version", 409)
	}
	// Compute every allocation first, then apply: a failure leaves no partial hold.
	type taken struct {
		b      *bucket
		amount *big.Int
	}
	var plan []taken
	heldNow := map[*bucket]*big.Int{}
	sorted := append([]router.Leg(nil), legs...)
	sort.Slice(sorted, func(i, j int) bool { return sorted[i].Account < sorted[j].Account })
	for _, leg := range sorted {
		remaining := atoms(leg.Reserved)
		if t.accountsByID[leg.Account] == nil {
			return router.Reservation{}, fail("insufficient_balance", 402)
		}
		var candidates []*bucket
		for _, b := range t.buckets {
			if b.accountID == leg.Account && (b.expiresAt == nil || *b.expiresAt > now) && (b.serviceIDs == nil || contains(b.serviceIDs, svc.ID)) {
				candidates = append(candidates, b)
			}
		}
		sort.SliceStable(candidates, func(i, j int) bool { return byExpiry(candidates[i], candidates[j]) })
		for _, b := range candidates {
			available := new(big.Int).Sub(b.remaining, b.held)
			if h := heldNow[b]; h != nil {
				available.Sub(available, h)
			}
			take := available
			if available.Cmp(remaining) >= 0 {
				take = new(big.Int).Set(remaining)
			}
			if take.Sign() <= 0 {
				continue
			}
			if heldNow[b] == nil {
				heldNow[b] = new(big.Int)
			}
			heldNow[b].Add(heldNow[b], take)
			plan = append(plan, taken{b, take})
			remaining = new(big.Int).Sub(remaining, take)
			if remaining.Sign() == 0 {
				break
			}
		}
		if remaining.Sign() > 0 {
			return router.Reservation{}, fail("insufficient_balance", 402)
		}
	}
	id := uuid.New()
	rec := router.RequestRecord{
		ID: id, Workspace: p.Workspace, Environment: p.Environment, Customer: p.Customer, Service: clone(svc),
		Execution: "accepted", Billing: "reserved", Deadline: deadline, Legs: append([]router.Leg(nil), legs...), ProviderSpent: "0",
	}
	r := &request{record: rec, digest: digest, providerSpent: new(big.Int), providerHeld: new(big.Int)}
	for _, tk := range plan {
		tk.b.held.Add(tk.b.held, tk.amount)
		r.allocations = append(r.allocations, &allocation{bucket: tk.b.id, account: tk.b.accountID, amount: tk.amount, charged: new(big.Int)})
	}
	for _, leg := range legs {
		a := t.accountsByID[leg.Account]
		a.held.Add(a.held, atoms(leg.Reserved))
	}
	t.requests[id] = r
	t.idem[key] = id
	return router.Reservation{Request: view(r), Fresh: true}, nil
}

// Begin implements router.Store.
func (s *Store) Begin(_ context.Context, p contracts.Principal, id string, o contracts.Offering, bound string) (string, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	t := s.tenant(p)
	r, err := t.row(p, id)
	if err != nil {
		return "", err
	}
	if r.record.CancelRequested || s.now() > r.record.Deadline.UnixMilli() {
		return "", fail("cancelled", 409)
	}
	if r.record.Billing != "reserved" {
		return "", fail("state_conflict", 409)
	}
	if t.disabled[o.ID] {
		return "", fail("connection_disabled", 409)
	}
	var snapshot *contracts.Offering
	for i := range r.record.Service.Offerings {
		if r.record.Service.Offerings[i].ID == o.ID {
			snapshot = &r.record.Service.Offerings[i]
		}
	}
	if snapshot == nil || !same(*snapshot, o) {
		return "", fail("state_conflict", 409)
	}
	b := atoms(bound)
	total := new(big.Int).Add(r.providerSpent, r.providerHeld)
	total.Add(total, b)
	if total.Cmp(atoms(r.record.Service.MaxProviderCost)) > 0 {
		return "", fail("budget_exceeded", 402)
	}
	a := &attempt{id: uuid.New(), offering: o.ID, status: "dispatched", bound: b}
	r.attempts = append(r.attempts, a)
	r.record.Execution = "running"
	r.providerHeld.Add(r.providerHeld, b)
	return a.id, nil
}

// Output implements router.Store.
func (s *Store) Output(_ context.Context, p contracts.Principal, id string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	r, err := s.tenant(p).row(p, id)
	if err != nil {
		return err
	}
	r.record.OutputStarted = true
	return nil
}

// AttemptResult implements router.Store. The first result of an attempt is final; later calls are ignored.
func (s *Store) AttemptResult(_ context.Context, p contracts.Principal, id, attemptID, status string, usage *contracts.Meters, cost *string, upstream string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	r, err := s.tenant(p).row(p, id)
	if err != nil {
		return err
	}
	if r.record.Billing != "reserved" && r.record.Billing != "pending" {
		return fail("state_conflict", 409)
	}
	var a *attempt
	for _, x := range r.attempts {
		if x.id == attemptID {
			a = x
		}
	}
	if a == nil {
		return fail("resource_not_found", 404)
	}
	if a.status != "dispatched" {
		return nil
	}
	a.status, a.usage, a.upstream = status, usage, upstream
	if cost != nil {
		a.cost = atoms(*cost)
		r.providerSpent.Add(r.providerSpent, a.cost)
		r.providerHeld.Sub(r.providerHeld, a.bound)
	}
	return nil
}

// Finish implements router.Store.
func (s *Store) Finish(_ context.Context, p contracts.Principal, id, execution string, usage *contracts.Meters, pending bool) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	t := s.tenant(p)
	r, err := t.row(p, id)
	if err != nil {
		return err
	}
	return s.finishTx(t, r, execution, usage, pending)
}

func (s *Store) event(t *tenant, id, kind string) {
	t.outbox = append(t.outbox, OutboxEvent{ID: uuid.New(), Type: kind, Request: id})
}

func (s *Store) finishTx(t *tenant, r *request, execution string, usage *contracts.Meters, pending bool) error {
	rec := &r.record
	if rec.Billing == "settled" || rec.Billing == "released" {
		return nil
	}
	if pending && !rec.OutputStarted && len(r.attempts) == 0 {
		pending, execution = false, "cancelled"
	}
	for _, a := range r.attempts {
		if usage != nil && a.cost != nil && a.cost.Cmp(a.bound) > 0 {
			pending = true
		}
	}
	due := make([]*big.Int, len(rec.Legs))
	exceeded := false
	for i, leg := range rec.Legs {
		due[i] = new(big.Int)
		if usage != nil {
			c, err := pricing.Charge(usage.Usage(), leg.Price)
			if err != nil {
				return err
			}
			due[i] = c
			if c.Cmp(atoms(leg.Reserved)) > 0 {
				exceeded = true
			}
		}
	}
	if pending || exceeded {
		if rec.Billing != "pending" {
			s.event(t, rec.ID, "reconciliation.required")
		}
		rec.Execution, rec.Billing = execution, "pending"
		if _, ok := t.reconciliation[rec.ID]; !ok {
			reason := "reservation_exceeded"
			if pending {
				reason = "unknown_outcome"
			}
			t.reconciliation[rec.ID] = reason
		}
		return nil
	}
	for i, leg := range rec.Legs {
		remaining := new(big.Int).Set(due[i])
		amount := new(big.Int).Set(due[i])
		type mine struct {
			a *allocation
			b *bucket
		}
		var own []mine
		for _, a := range r.allocations {
			if a.account != leg.Account {
				continue
			}
			for _, b := range t.buckets {
				if b.id == a.bucket {
					own = append(own, mine{a, b})
				}
			}
		}
		sort.SliceStable(own, func(x, y int) bool { return byExpiry(own[x].b, own[y].b) })
		for _, m := range own {
			take := remaining
			if m.a.amount.Cmp(remaining) < 0 {
				take = m.a.amount
			}
			take = new(big.Int).Set(take)
			remaining.Sub(remaining, take)
			m.a.charged = take
			m.b.remaining.Sub(m.b.remaining, take)
			m.b.held.Sub(m.b.held, m.a.amount)
		}
		a := t.accountsByID[leg.Account]
		a.balance.Sub(a.balance, amount)
		a.held.Sub(a.held, atoms(leg.Reserved))
		t.journal = append(t.journal, JournalEntry{Key: "settle:" + rec.ID + ":" + leg.Kind, Asset: leg.Price.Asset, Debit: leg.Account, Credit: "consumed:" + leg.Price.Asset, Amount: amount.String(), Reason: "settlement"})
	}
	rec.Execution = execution
	rec.Billing = "released"
	kind := "billing.released"
	if usage != nil {
		rec.Billing, kind = "settled", "billing.settled"
	}
	s.expire(t)
	s.event(t, rec.ID, kind)
	return nil
}

// expire moves the unheld remainder of expired buckets out of the balance.
func (s *Store) expire(t *tenant) {
	now := s.now()
	for _, b := range t.buckets {
		if b.expiresAt == nil || *b.expiresAt > now || b.remaining.Cmp(b.held) <= 0 {
			continue
		}
		n := new(big.Int).Sub(b.remaining, b.held)
		b.remaining.Set(b.held)
		a := t.accountsByID[b.accountID]
		a.balance.Sub(a.balance, n)
		t.journal = append(t.journal, JournalEntry{Key: "expire:" + s.id("x_"), Asset: a.asset, Debit: b.accountID, Credit: "expired:" + a.asset, Amount: n.String(), Reason: "expiry"})
	}
}

// Balance implements router.DataStore.
func (s *Store) Balance(_ context.Context, p contracts.Principal) ([]router.BalanceRow, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	t := s.tenant(p)
	s.expire(t)
	var accounts []*account
	for _, a := range t.accounts {
		if a.owner == p.Customer || a.owner == p.Workspace && a.kind == "platform_sale" {
			accounts = append(accounts, a)
		}
	}
	sort.Slice(accounts, func(i, j int) bool {
		if accounts[i].kind != accounts[j].kind {
			return accounts[i].kind < accounts[j].kind
		}
		return accounts[i].id < accounts[j].id
	})
	rows := []router.BalanceRow{}
	for _, a := range accounts {
		rows = append(rows, router.BalanceRow{
			ID: a.id, Kind: a.kind, Asset: a.asset, Balance: a.balance.String(), Held: a.held.String(),
			Available: new(big.Int).Sub(a.balance, a.held).String(),
		})
	}
	return rows, nil
}

// Cancel implements router.Store.
func (s *Store) Cancel(_ context.Context, p contracts.Principal, id string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	r, err := s.tenant(p).row(p, id)
	if err != nil {
		return err
	}
	r.record.CancelRequested = true
	return nil
}

// Disable implements router.DataStore.
func (s *Store) Disable(_ context.Context, p contracts.Principal, offering string) error {
	if !p.HasScope("service:write") {
		return fail("scope_denied", 403)
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	s.tenant(p).disabled[offering] = true
	return nil
}

// Recover implements router.DataStore. It never redispatches an expired uncertain attempt.
func (s *Store) Recover(_ context.Context) (int, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	n := 0
	now := s.now()
	for _, t := range s.tenants {
		ids := make([]string, 0, len(t.requests))
		for id := range t.requests {
			ids = append(ids, id)
		}
		sort.Strings(ids)
		for _, id := range ids {
			r := t.requests[id]
			if r.record.Billing != "reserved" || r.record.Deadline.UnixMilli() >= now {
				continue
			}
			var last *attempt
			if len(r.attempts) > 0 {
				last = r.attempts[len(r.attempts)-1]
			}
			known := last != nil && (last.status == "succeeded" || last.status == "partial") && last.usage != nil
			execution, usage := "cancelled", (*contracts.Meters)(nil)
			switch {
			case known:
				execution, usage = last.status, last.usage
			case last != nil:
				execution = "unknown"
			}
			if err := s.finishTx(t, r, execution, usage, last != nil && !known); err != nil {
				return n, err
			}
			n++
		}
	}
	return n, nil
}

// Entitle lets a customer use subscription-only services for a period. The control plane does this in the SQL store.
func (s *Store) Entitle(p contracts.Principal, serviceIDs []string, start, end time.Time) {
	s.mu.Lock()
	defer s.mu.Unlock()
	t := s.tenant(p)
	t.entitlements = append(t.entitlements, entitlement{customer: p.Customer, serviceIDs: serviceIDs, start: start.UnixMilli(), end: end.UnixMilli()})
}

// Inspect returns a read-only snapshot of a tenant for tests and diagnostics.
func (s *Store) Inspect(p contracts.Principal) Inspection {
	s.mu.Lock()
	defer s.mu.Unlock()
	t := s.tenant(p)
	out := Inspection{
		Journal:        append([]JournalEntry(nil), t.journal...),
		Outbox:         append([]OutboxEvent(nil), t.outbox...),
		Reconciliation: map[string]string{},
	}
	for k, v := range t.reconciliation {
		out.Reconciliation[k] = v
	}
	for _, b := range t.buckets {
		out.Buckets = append(out.Buckets, BucketView{Source: b.source, Remaining: b.remaining.String(), Held: b.held.String()})
	}
	return out
}
