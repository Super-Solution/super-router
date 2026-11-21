// SPDX-License-Identifier: Apache-2.0

// Package postgres is the PostgreSQL implementation of router.DataStore.
//
// It uses only database/sql, so any driver works: pgx (github.com/jackc/pgx/v5/stdlib), lib/pq, or a driver
// wrapped by your ORM's connection pool. The schema is shared with the TypeScript store, so both can serve the
// same database. Every operation runs in one transaction that first takes an advisory lock on the tenant
// (workspace and environment), which serialises admission, holds and settlement per tenant.
package postgres

import (
	"context"
	"database/sql"
	"embed"
	"encoding/json"
	"errors"
	"fmt"
	"math/big"
	"sort"
	"time"

	"github.com/Super-Solution/super-router/backends/go/contracts"
	"github.com/Super-Solution/super-router/backends/go/internal/uuid"
	"github.com/Super-Solution/super-router/backends/go/pricing"
	"github.com/Super-Solution/super-router/backends/go/router"
)

//go:embed migrations/*.sql
var migrationFiles embed.FS

// Options configures a Store.
type Options struct {
	// Clock is used for the dispatch deadline check. Nil means time.Now. SQL expiry always uses the database clock.
	Clock router.Clock
	// Revalidate, when set, is called inside each write transaction before it changes anything, with the scope
	// the operation needs ("service:write", "billing:write" or "invoke"). Hosts that authenticate with
	// database-backed keys use it to re-check that the key was not revoked or narrowed since it was verified.
	Revalidate func(ctx context.Context, tx *sql.Tx, p contracts.Principal, scope string) error
}

// Store implements router.DataStore on PostgreSQL.
type Store struct {
	db    *sql.DB
	clock router.Clock
	opts  Options
}

var _ router.DataStore = (*Store)(nil)

// New creates a store on an open pool. The caller owns the pool and closes it.
func New(db *sql.DB, opts Options) *Store {
	clock := opts.Clock
	if clock == nil {
		clock = time.Now
	}
	return &Store{db: db, clock: clock, opts: opts}
}

// DB exposes the pool, for hosts that add their own tables or queries beside the router's.
func (s *Store) DB() *sql.DB { return s.db }

func fail(code string, status int) error { return contracts.NewError(code, status) }

type execer interface {
	ExecContext(ctx context.Context, query string, args ...any) (sql.Result, error)
	QueryContext(ctx context.Context, query string, args ...any) (*sql.Rows, error)
	QueryRowContext(ctx context.Context, query string, args ...any) *sql.Row
}

// Migrate applies the embedded migrations that have not run yet, under an advisory lock so several instances
// can start at once. The migration files are byte-identical to the TypeScript store's.
func (s *Store) Migrate(ctx context.Context) error {
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return err
	}
	defer tx.Rollback()
	if _, err := tx.ExecContext(ctx, "SELECT pg_advisory_xact_lock(hashtextextended('superrouter:migrate',0))"); err != nil {
		return err
	}
	if _, err := tx.ExecContext(ctx, "CREATE TABLE IF NOT EXISTS sr_migrations(version integer PRIMARY KEY)"); err != nil {
		return err
	}
	entries, err := migrationFiles.ReadDir("migrations")
	if err != nil {
		return err
	}
	names := make([]string, 0, len(entries))
	for _, e := range entries {
		names = append(names, e.Name())
	}
	sort.Strings(names)
	for _, name := range names {
		var version int
		if _, err := fmt.Sscanf(name, "%03d_", &version); err != nil {
			return fmt.Errorf("postgres: migration %q has no version prefix", name)
		}
		var done int
		err := tx.QueryRowContext(ctx, "SELECT 1 FROM sr_migrations WHERE version=$1", version).Scan(&done)
		if err == nil {
			continue
		}
		if !errors.Is(err, sql.ErrNoRows) {
			return err
		}
		body, err := migrationFiles.ReadFile("migrations/" + name)
		if err != nil {
			return err
		}
		if _, err := tx.ExecContext(ctx, string(body)); err != nil {
			return fmt.Errorf("postgres: migration %s: %w", name, err)
		}
		if _, err := tx.ExecContext(ctx, "INSERT INTO sr_migrations VALUES($1)", version); err != nil {
			return err
		}
	}
	return tx.Commit()
}

func (s *Store) tx(ctx context.Context, p contracts.Principal, fn func(tx *sql.Tx) error) error {
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return err
	}
	defer tx.Rollback()
	scope, _ := json.Marshal([]string{p.Workspace, p.Environment})
	if _, err := tx.ExecContext(ctx, "SELECT pg_advisory_xact_lock(hashtextextended($1,0))", string(scope)); err != nil {
		return err
	}
	if err := fn(tx); err != nil {
		return err
	}
	return tx.Commit()
}

func (s *Store) revalidate(ctx context.Context, tx *sql.Tx, p contracts.Principal, scope string) error {
	if s.opts.Revalidate != nil {
		if err := s.opts.Revalidate(ctx, tx, p, scope); err != nil {
			return err
		}
	}
	if scope != "" && !p.HasScope(scope) {
		return fail("scope_denied", 403)
	}
	return nil
}

func atoms(s string) *big.Int {
	n, ok := new(big.Int).SetString(s, 10)
	if !ok {
		panic("postgres: not a decimal integer: " + s)
	}
	return n
}

func same(a, b any) bool {
	x, _ := json.Marshal(a)
	y, _ := json.Marshal(b)
	return string(x) == string(y)
}

// strings converts a text[] column selected as JSON.
func jsonStrings(raw []byte) []string {
	if len(raw) == 0 || string(raw) == "null" {
		return nil
	}
	var out []string
	_ = json.Unmarshal(raw, &out)
	return out
}

// arrayArg passes a Go slice as JSON and lets SQL build the text[]; it works with every driver.
func arrayArg(list []string) sql.NullString {
	if list == nil {
		return sql.NullString{}
	}
	raw, _ := json.Marshal(list)
	return sql.NullString{String: string(raw), Valid: true}
}

const arrayExpr = "(SELECT array_agg(x) FROM jsonb_array_elements_text(%s::jsonb) x)"

const requestColumns = "id,customer,digest,service,legs,execution,billing,cancel_requested,output_started,provider_spent::text,deadline"

func scanRequest(workspace, environment string, row interface{ Scan(...any) error }) (*router.RequestRecord, string, error) {
	var (
		rec        router.RequestRecord
		serviceRaw []byte
		legsRaw    []byte
		digest     string
	)
	if err := row.Scan(&rec.ID, &rec.Customer, &digest, &serviceRaw, &legsRaw, &rec.Execution, &rec.Billing, &rec.CancelRequested, &rec.OutputStarted, &rec.ProviderSpent, &rec.Deadline); err != nil {
		return nil, "", err
	}
	service, err := contracts.ParseService(serviceRaw)
	if err != nil {
		return nil, "", fmt.Errorf("postgres: stored service of request %s is invalid: %w", rec.ID, err)
	}
	rec.Service = service
	if err := json.Unmarshal(legsRaw, &rec.Legs); err != nil {
		return nil, "", err
	}
	rec.Workspace, rec.Environment = workspace, environment
	return &rec, digest, nil
}

func (s *Store) row(ctx context.Context, q execer, p contracts.Principal, id string) (*router.RequestRecord, error) {
	rec, _, err := scanRequest(p.Workspace, p.Environment, q.QueryRowContext(ctx,
		"SELECT "+requestColumns+" FROM sr_requests WHERE workspace=$1 AND environment=$2 AND id=$3 AND customer=$4",
		p.Workspace, p.Environment, id, p.Customer))
	if errors.Is(err, sql.ErrNoRows) {
		return nil, fail("resource_not_found", 404)
	}
	if err != nil {
		return nil, err
	}
	if !p.MayUse(rec.Service.ID) {
		return nil, fail("scope_denied", 403)
	}
	return rec, nil
}

// Replay implements router.Store.
func (s *Store) Replay(ctx context.Context, p contracts.Principal, key, digest string) (*router.RequestRecord, error) {
	rec, stored, err := scanRequest(p.Workspace, p.Environment, s.db.QueryRowContext(ctx,
		"SELECT "+requestColumns+" FROM sr_requests WHERE workspace=$1 AND environment=$2 AND idem=$3", p.Workspace, p.Environment, key))
	if errors.Is(err, sql.ErrNoRows) {
		return nil, nil
	}
	if err != nil {
		return nil, err
	}
	if stored != digest || rec.Customer != p.Customer {
		return nil, fail("idempotency_conflict", 409)
	}
	return rec, nil
}

// Get implements router.Store.
func (s *Store) Get(ctx context.Context, p contracts.Principal, id string) (*router.RequestRecord, error) {
	return s.row(ctx, s.db, p, id)
}

func parseStored(raw []byte) (contracts.Service, error) {
	svc, err := contracts.ParseService(raw)
	if err != nil {
		return contracts.Service{}, fmt.Errorf("postgres: stored service is invalid: %w", err)
	}
	return svc, nil
}

// Publish implements router.DataStore.
func (s *Store) Publish(ctx context.Context, p contracts.Principal, svc contracts.Service, mode string, expectedVersion *int64) (contracts.Service, error) {
	if !p.HasScope("service:write") || !p.MayUse(svc.ID) {
		return contracts.Service{}, fail("scope_denied", 403)
	}
	if mode == "" {
		mode = "self-host"
	}
	if mode == "cloud" {
		for _, o := range svc.AllOfferings() {
			if o.Source == "byok" {
				return contracts.Service{}, fail("byok_not_allowed", 403)
			}
		}
	}
	config, err := json.Marshal(svc)
	if err != nil {
		return contracts.Service{}, err
	}
	err = s.tx(ctx, p, func(tx *sql.Tx) error {
		if err := s.revalidate(ctx, tx, p, "service:write"); err != nil {
			return err
		}
		var previous *contracts.Service
		var raw []byte
		switch err := tx.QueryRowContext(ctx,
			"SELECT config FROM sr_services WHERE workspace=$1 AND environment=$2 AND id=$3 ORDER BY version DESC LIMIT 1",
			p.Workspace, p.Environment, svc.ID).Scan(&raw); {
		case errors.Is(err, sql.ErrNoRows):
		case err != nil:
			return err
		default:
			parsed, err := parseStored(raw)
			if err != nil {
				return err
			}
			previous = &parsed
		}
		if expectedVersion != nil && (previous == nil || previous.Version != *expectedVersion) {
			return fail("service_version_conflict", 409)
		}
		if mode == "cloud" && p.AuthKeyID != "" {
			// Database-authenticated developers cannot provision or rewrite platform supply economics.
			if previous == nil || !same(previous.PlatformPrice, svc.PlatformPrice) || !same(previous.AllOfferings(), svc.AllOfferings()) {
				return fail("platform_pricing_protected", 403)
			}
		}
		switch err := tx.QueryRowContext(ctx,
			"SELECT config FROM sr_services WHERE workspace=$1 AND environment=$2 AND id=$3 AND version=$4",
			p.Workspace, p.Environment, svc.ID, svc.Version).Scan(&raw); {
		case err == nil:
			existing, err := parseStored(raw)
			if err != nil {
				return err
			}
			if !same(existing, svc) {
				return fail("version_exists", 409)
			}
			return nil
		case !errors.Is(err, sql.ErrNoRows):
			return err
		}
		if previous != nil && svc.Version <= previous.Version {
			return fail("service_version_conflict", 409)
		}
		if _, err := tx.ExecContext(ctx, "UPDATE sr_services SET active=false WHERE workspace=$1 AND environment=$2 AND id=$3", p.Workspace, p.Environment, svc.ID); err != nil {
			return err
		}
		if _, err := tx.ExecContext(ctx, "INSERT INTO sr_services(workspace,environment,id,version,config) VALUES($1,$2,$3,$4,$5::jsonb)",
			p.Workspace, p.Environment, svc.ID, svc.Version, string(config)); err != nil {
			return err
		}
		actor := p.MemberID
		if actor == "" {
			actor = "trusted-host"
		}
		details, _ := json.Marshal(map[string]int64{"version": svc.Version})
		_, err := tx.ExecContext(ctx, "INSERT INTO sr_audit(id,workspace,environment,actor,action,resource,details) VALUES($1,$2,$3,$4,$5,$6,$7::jsonb)",
			uuid.New(), p.Workspace, p.Environment, actor, "service.published", svc.ID, string(details))
		return err
	})
	if err != nil {
		return contracts.Service{}, err
	}
	return svc, nil
}

// GetService implements router.Store.
func (s *Store) GetService(ctx context.Context, p contracts.Principal, id string, version *int64) (contracts.Service, error) {
	if !p.MayUse(id) {
		return contracts.Service{}, fail("scope_denied", 403)
	}
	query := "SELECT config FROM sr_services WHERE workspace=$1 AND environment=$2 AND id=$3 AND active=true"
	args := []any{p.Workspace, p.Environment, id}
	if version != nil {
		query = "SELECT config FROM sr_services WHERE workspace=$1 AND environment=$2 AND id=$3 AND version=$4"
		args = append(args, *version)
	}
	var raw []byte
	err := s.db.QueryRowContext(ctx, query, args...).Scan(&raw)
	if errors.Is(err, sql.ErrNoRows) {
		return contracts.Service{}, fail("resource_not_found", 404)
	}
	if err != nil {
		return contracts.Service{}, err
	}
	return parseStored(raw)
}

// List implements router.DataStore.
func (s *Store) List(ctx context.Context, p contracts.Principal) ([]contracts.Service, error) {
	rows, err := s.db.QueryContext(ctx, "SELECT config FROM sr_services WHERE workspace=$1 AND environment=$2 AND active=true ORDER BY id LIMIT 100", p.Workspace, p.Environment)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	out := []contracts.Service{}
	for rows.Next() {
		var raw []byte
		if err := rows.Scan(&raw); err != nil {
			return nil, err
		}
		svc, err := parseStored(raw)
		if err != nil {
			return nil, err
		}
		if p.MayUse(svc.ID) {
			out = append(out, svc)
		}
	}
	return out, rows.Err()
}

// GrantOptions are the optional parts of a grant.
type GrantOptions struct {
	Expires    *time.Time
	ServiceIDs []string // nil means every service
	SourceKind string   // purchased (default), promotion or subscription
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

// Grant implements router.DataStore.
func (s *Store) Grant(ctx context.Context, p contracts.Principal, kind, asset, amount, source string, expires *time.Time) (string, error) {
	return s.GrantWith(ctx, p, kind, asset, amount, source, GrantOptions{Expires: expires})
}

// GrantWith is Grant with the full set of options.
func (s *Store) GrantWith(ctx context.Context, p contracts.Principal, kind, asset, amount, source string, opt GrantOptions) (string, error) {
	if !p.HasScope("billing:write") {
		return "", fail("scope_denied", 403)
	}
	if !validAmount(amount) {
		return "", fail("invalid_amount", 400)
	}
	var account string
	err := s.tx(ctx, p, func(tx *sql.Tx) error {
		if err := s.revalidate(ctx, tx, p, "billing:write"); err != nil {
			return err
		}
		var err error
		account, err = s.GrantTx(ctx, tx, p, kind, asset, amount, source, opt)
		return err
	})
	return account, err
}

// GrantTx adds a funding bucket inside a transaction the caller owns, after the caller has authorized it.
// Control planes use it to grant credits together with their own writes.
func (s *Store) GrantTx(ctx context.Context, tx *sql.Tx, p contracts.Principal, kind, asset, amount, source string, opt GrantOptions) (string, error) {
	if opt.SourceKind == "" {
		opt.SourceKind = "purchased"
	}
	var expires sql.NullTime
	if opt.Expires != nil {
		expires = sql.NullTime{Time: opt.Expires.UTC().Truncate(time.Millisecond), Valid: true}
	}
	owner := p.Customer
	if kind == "platform_sale" {
		owner = p.Workspace
	}
	var (
		bAccount, bAsset, bOwner, bKind, bGranted, bSourceKind string
		bExpires                                               sql.NullTime
		bServiceIDs                                            []byte
	)
	err := tx.QueryRowContext(ctx,
		"SELECT b.account,a.asset,a.owner,a.kind,b.granted::text,b.expires_at,to_json(b.service_ids)::text,b.source_kind FROM sr_buckets b JOIN sr_accounts a ON (a.workspace,a.environment,a.id)=(b.workspace,b.environment,b.account) WHERE b.workspace=$1 AND b.environment=$2 AND b.source=$3",
		p.Workspace, p.Environment, source).Scan(&bAccount, &bAsset, &bOwner, &bKind, &bGranted, &bExpires, &bServiceIDs, &bSourceKind)
	switch {
	case err == nil:
		if bGranted != amount || bAsset != asset || bOwner != owner || bKind != kind ||
			bExpires.Valid != expires.Valid || bExpires.Valid && !bExpires.Time.UTC().Truncate(time.Millisecond).Equal(expires.Time) ||
			!same(jsonStrings(bServiceIDs), opt.ServiceIDs) || bSourceKind != opt.SourceKind {
			return "", fail("idempotency_conflict", 409)
		}
		return bAccount, nil
	case !errors.Is(err, sql.ErrNoRows):
		return "", err
	}
	if _, err := tx.ExecContext(ctx,
		"INSERT INTO sr_accounts(workspace,environment,id,owner,kind,asset) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT(workspace,environment,owner,kind,asset) DO NOTHING",
		p.Workspace, p.Environment, uuid.New(), owner, kind, asset); err != nil {
		return "", err
	}
	var account string
	if err := tx.QueryRowContext(ctx, "SELECT id FROM sr_accounts WHERE workspace=$1 AND environment=$2 AND owner=$3 AND kind=$4 AND asset=$5",
		p.Workspace, p.Environment, owner, kind, asset).Scan(&account); err != nil {
		return "", err
	}
	if _, err := tx.ExecContext(ctx, "UPDATE sr_accounts SET balance=balance+$4::numeric WHERE workspace=$1 AND environment=$2 AND id=$3", p.Workspace, p.Environment, account, amount); err != nil {
		return "", err
	}
	if _, err := tx.ExecContext(ctx,
		"INSERT INTO sr_buckets(workspace,environment,id,account,source,expires_at,remaining,granted,service_ids,source_kind) VALUES($1,$2,$3,$4,$5,$6,$7::numeric,$7::numeric,"+fmt.Sprintf(arrayExpr, "$8")+",$9)",
		p.Workspace, p.Environment, uuid.New(), account, source, expires, amount, arrayArg(opt.ServiceIDs), opt.SourceKind); err != nil {
		return "", err
	}
	if _, err := tx.ExecContext(ctx,
		"INSERT INTO sr_journal(workspace,environment,business_key,asset,debit,credit,amount,reason) VALUES($1,$2,$3,$4,$5,$6,$7::numeric,$8)",
		p.Workspace, p.Environment, "grant:"+source, asset, "issuer:"+asset, account, amount, "grant"); err != nil {
		return "", err
	}
	return account, nil
}

// Accounts implements router.Store.
func (s *Store) Accounts(ctx context.Context, p contracts.Principal, svc contracts.Service) ([]router.Leg, error) {
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
		var id string
		err := s.db.QueryRowContext(ctx, "SELECT id FROM sr_accounts WHERE workspace=$1 AND environment=$2 AND owner=$3 AND kind=$4 AND asset=$5",
			p.Workspace, p.Environment, owner, entry.kind, entry.price.Asset).Scan(&id)
		if errors.Is(err, sql.ErrNoRows) {
			return nil, fail("insufficient_balance", 402)
		}
		if err != nil {
			return nil, err
		}
		legs = append(legs, router.Leg{Kind: entry.kind, Account: id, Price: *entry.price, Reserved: "0"})
	}
	return legs, nil
}

// Reserve implements router.Store.
func (s *Store) Reserve(ctx context.Context, p contracts.Principal, key, digest string, svc contracts.Service, legs []router.Leg, deadline time.Time, decision router.Decision) (router.Reservation, error) {
	var out router.Reservation
	err := s.tx(ctx, p, func(tx *sql.Tx) error {
		if err := s.revalidate(ctx, tx, p, "invoke"); err != nil {
			return err
		}
		if !p.MayUse(svc.ID) {
			return fail("scope_denied", 403)
		}
		if svc.RequiresSubscription != nil && *svc.RequiresSubscription {
			var one int
			err := tx.QueryRowContext(ctx,
				"SELECT 1 FROM sr_entitlements WHERE workspace=$1 AND environment=$2 AND customer=$3 AND $4=ANY(service_ids) AND period_start<=now() AND period_end>now() LIMIT 1",
				p.Workspace, p.Environment, p.Customer, svc.ID).Scan(&one)
			if errors.Is(err, sql.ErrNoRows) {
				return fail("subscription_required", 403)
			}
			if err != nil {
				return err
			}
		}
		old, storedDigest, err := scanRequest(p.Workspace, p.Environment, tx.QueryRowContext(ctx,
			"SELECT "+requestColumns+" FROM sr_requests WHERE workspace=$1 AND environment=$2 AND idem=$3", p.Workspace, p.Environment, key))
		switch {
		case err == nil:
			if storedDigest != digest || old.Customer != p.Customer {
				return fail("idempotency_conflict", 409)
			}
			out = router.Reservation{Request: old, Fresh: false}
			return nil
		case !errors.Is(err, sql.ErrNoRows):
			return err
		}
		// Admission and service publication share the tenant lock. Replays keep their admitted snapshot;
		// fresh work must reserve against the active version.
		var one int
		err = tx.QueryRowContext(ctx, "SELECT 1 FROM sr_services WHERE workspace=$1 AND environment=$2 AND id=$3 AND version=$4 AND active=true",
			p.Workspace, p.Environment, svc.ID, svc.Version).Scan(&one)
		if errors.Is(err, sql.ErrNoRows) {
			return fail("inactive_service_version", 409)
		}
		if err != nil {
			return err
		}
		id := uuid.New()
		serviceJSON, _ := json.Marshal(svc)
		legsJSON, _ := json.Marshal(legs)
		decisionJSON, _ := json.Marshal(decision)
		if _, err := tx.ExecContext(ctx,
			"INSERT INTO sr_requests(workspace,environment,id,customer,idem,digest,service,legs,deadline,route_decision) VALUES($1,$2,$3,$4,$5,$6,$7::jsonb,$8::jsonb,$9,$10::jsonb)",
			p.Workspace, p.Environment, id, p.Customer, key, digest, string(serviceJSON), string(legsJSON), deadline, string(decisionJSON)); err != nil {
			return err
		}
		ordered := append([]router.Leg(nil), legs...)
		sort.Slice(ordered, func(i, j int) bool { return ordered[i].Account < ordered[j].Account })
		for _, leg := range ordered {
			remaining := atoms(leg.Reserved)
			type bucketRow struct {
				id              string
				remaining, held string
				subscription    sql.NullString
				windowLimits    sql.NullString
			}
			var blocked *windowBlock
			// A subscription bucket may carry session/weekly window limits (its entitlement's snapshot). They
			// gate only that bucket: once its windows are full the draw falls through to the next bucket
			// (purchased or promotional credits) as for an empty one.
			rows, err := tx.QueryContext(ctx,
				"SELECT b.id,b.remaining::text,b.held::text,e.subscription,e.window_limits::text FROM sr_buckets b LEFT JOIN sr_entitlements e ON (e.workspace,e.environment,e.bucket)=(b.workspace,b.environment,b.id) WHERE b.workspace=$1 AND b.environment=$2 AND b.account=$3 AND (b.expires_at IS NULL OR b.expires_at>now()) AND (b.service_ids IS NULL OR $4=ANY(b.service_ids)) ORDER BY b.expires_at ASC NULLS LAST, CASE b.source_kind WHEN 'subscription' THEN 0 WHEN 'promotion' THEN 1 ELSE 2 END,b.id FOR UPDATE OF b",
				p.Workspace, p.Environment, leg.Account, svc.ID)
			if err != nil {
				return err
			}
			var buckets []bucketRow
			for rows.Next() {
				var b bucketRow
				if err := rows.Scan(&b.id, &b.remaining, &b.held, &b.subscription, &b.windowLimits); err != nil {
					rows.Close()
					return err
				}
				buckets = append(buckets, b)
			}
			if err := rows.Close(); err != nil {
				return err
			}
			for _, b := range buckets {
				available := new(big.Int).Sub(atoms(b.remaining), atoms(b.held))
				take := available
				if available.Cmp(remaining) >= 0 {
					take = new(big.Int).Set(remaining)
				}
				if take.Sign() <= 0 {
					continue
				}
				var windowIDs []string
				if b.subscription.Valid {
					specs, err := windowSpecs(b.windowLimits)
					if err != nil {
						return err
					}
					if len(specs) > 0 {
						admitted, ids, gate, err := s.admit(ctx, tx, p, b.subscription.String, specs, take)
						if err != nil {
							return err
						}
						if gate != nil {
							blocked = gate
						}
						take, windowIDs = admitted, ids
						if take.Sign() <= 0 {
							continue
						}
					}
				}
				if _, err := tx.ExecContext(ctx, "UPDATE sr_buckets SET held=held+$4::numeric WHERE workspace=$1 AND environment=$2 AND id=$3", p.Workspace, p.Environment, b.id, take.String()); err != nil {
					return err
				}
				if _, err := tx.ExecContext(ctx, "INSERT INTO sr_allocations(workspace,environment,request,bucket,account,amount,windows) VALUES($1,$2,$3,$4,$5,$6::numeric,COALESCE("+fmt.Sprintf(arrayExpr, "$7")+",'{}'))",
					p.Workspace, p.Environment, id, b.id, leg.Account, take.String(), arrayArg(append([]string{}, windowIDs...))); err != nil {
					return err
				}
				remaining = new(big.Int).Sub(remaining, take)
				if remaining.Sign() == 0 {
					break
				}
			}
			if remaining.Sign() > 0 {
				if blocked != nil {
					return windowError(blocked)
				}
				return fail("insufficient_balance", 402)
			}
			if _, err := tx.ExecContext(ctx, "UPDATE sr_accounts SET held=held+$4::numeric WHERE workspace=$1 AND environment=$2 AND id=$3", p.Workspace, p.Environment, leg.Account, leg.Reserved); err != nil {
				return err
			}
		}
		rec, err := s.row(ctx, tx, p, id)
		if err != nil {
			return err
		}
		out = router.Reservation{Request: rec, Fresh: true}
		return nil
	})
	return out, err
}

// Begin implements router.Store.
func (s *Store) Begin(ctx context.Context, p contracts.Principal, id string, o contracts.Offering, bound string) (string, error) {
	var attempt string
	err := s.tx(ctx, p, func(tx *sql.Tx) error {
		if err := s.revalidate(ctx, tx, p, "invoke"); err != nil {
			return err
		}
		r, err := s.row(ctx, tx, p, id)
		if err != nil {
			return err
		}
		if r.CancelRequested || s.clock().After(r.Deadline) {
			return fail("cancelled", 409)
		}
		if r.Billing != "reserved" {
			return fail("state_conflict", 409)
		}
		var one int
		switch err := tx.QueryRowContext(ctx, "SELECT 1 FROM sr_disabled_connections WHERE workspace=$1 AND environment=$2 AND offering=$3", p.Workspace, p.Environment, o.ID).Scan(&one); {
		case err == nil:
			return fail("connection_disabled", 409)
		case !errors.Is(err, sql.ErrNoRows):
			return err
		}
		var snapshot *contracts.Offering
		offerings := r.Service.AllOfferings()
		for i := range offerings {
			if offerings[i].ID == o.ID {
				snapshot = &offerings[i]
			}
		}
		if snapshot == nil || !same(*snapshot, o) {
			return fail("state_conflict", 409)
		}
		var total string
		if err := tx.QueryRowContext(ctx, "SELECT (provider_spent+provider_held+$4::numeric)::text FROM sr_requests WHERE workspace=$1 AND environment=$2 AND id=$3",
			p.Workspace, p.Environment, id, bound).Scan(&total); err != nil {
			return err
		}
		if atoms(total).Cmp(atoms(r.Service.MaxProviderCost)) > 0 {
			return fail("budget_exceeded", 402)
		}
		attempt = uuid.New()
		if _, err := tx.ExecContext(ctx, "INSERT INTO sr_attempts(workspace,environment,id,request,offering,cost_bound) VALUES($1,$2,$3,$4,$5,$6::numeric)",
			p.Workspace, p.Environment, attempt, id, o.ID, bound); err != nil {
			return err
		}
		_, err = tx.ExecContext(ctx, "UPDATE sr_requests SET execution='running',provider_held=provider_held+$4::numeric WHERE workspace=$1 AND environment=$2 AND id=$3", p.Workspace, p.Environment, id, bound)
		return err
	})
	return attempt, err
}

// Output implements router.Store.
func (s *Store) Output(ctx context.Context, p contracts.Principal, id string) error {
	return s.tx(ctx, p, func(tx *sql.Tx) error {
		if _, err := s.row(ctx, tx, p, id); err != nil {
			return err
		}
		_, err := tx.ExecContext(ctx, "UPDATE sr_requests SET output_started=true WHERE workspace=$1 AND environment=$2 AND id=$3", p.Workspace, p.Environment, id)
		return err
	})
}

// AttemptResult implements router.Store. The first result of an attempt is final; later calls are ignored.
func (s *Store) AttemptResult(ctx context.Context, p contracts.Principal, id, attempt, status string, usage *contracts.Meters, cost *string, upstream string) error {
	return s.tx(ctx, p, func(tx *sql.Tx) error {
		r, err := s.row(ctx, tx, p, id)
		if err != nil {
			return err
		}
		if r.Billing != "reserved" && r.Billing != "pending" {
			return fail("state_conflict", 409)
		}
		var current, bound string
		err = tx.QueryRowContext(ctx, "SELECT status,cost_bound::text FROM sr_attempts WHERE workspace=$1 AND environment=$2 AND id=$3 AND request=$4 FOR UPDATE",
			p.Workspace, p.Environment, attempt, id).Scan(&current, &bound)
		if errors.Is(err, sql.ErrNoRows) {
			return fail("resource_not_found", 404)
		}
		if err != nil {
			return err
		}
		if current != "dispatched" {
			return nil
		}
		var usageArg, costArg, upstreamArg sql.NullString
		if usage != nil {
			raw, _ := json.Marshal(usage)
			usageArg = sql.NullString{String: string(raw), Valid: true}
		}
		if cost != nil {
			costArg = sql.NullString{String: *cost, Valid: true}
		}
		if upstream != "" {
			upstreamArg = sql.NullString{String: upstream, Valid: true}
		}
		if _, err := tx.ExecContext(ctx, "UPDATE sr_attempts SET status=$4,usage=$5::jsonb,cost=$6::numeric,upstream_id=$7 WHERE workspace=$1 AND environment=$2 AND id=$3",
			p.Workspace, p.Environment, attempt, status, usageArg, costArg, upstreamArg); err != nil {
			return err
		}
		if cost != nil {
			_, err = tx.ExecContext(ctx, "UPDATE sr_requests SET provider_spent=provider_spent+$4::numeric,provider_held=provider_held-$5::numeric WHERE workspace=$1 AND environment=$2 AND id=$3",
				p.Workspace, p.Environment, id, *cost, bound)
		}
		return err
	})
}

func (s *Store) event(ctx context.Context, tx *sql.Tx, p contracts.Principal, id, kind string) error {
	eventID := uuid.New()
	payload, _ := json.Marshal(map[string]any{
		"id": eventID, "type": kind, "schema_version": 1, "workspace_id": p.Workspace, "environment_id": p.Environment,
		"request_id": id, "occurred_at": time.Now().UTC().Format("2006-01-02T15:04:05.000Z"),
	})
	_, err := tx.ExecContext(ctx, "INSERT INTO sr_outbox(id,workspace,environment,request,payload) VALUES($1,$2,$3,$4,$5::jsonb)", eventID, p.Workspace, p.Environment, id, string(payload))
	return err
}

// Finish implements router.Store.
func (s *Store) Finish(ctx context.Context, p contracts.Principal, id, execution string, usage *contracts.Meters, pending bool) error {
	return s.tx(ctx, p, func(tx *sql.Tx) error { return s.finishTx(ctx, tx, p, id, execution, usage, pending) })
}

func (s *Store) finishTx(ctx context.Context, tx *sql.Tx, p contracts.Principal, id, execution string, usage *contracts.Meters, pending bool) error {
	r, err := s.row(ctx, tx, p, id)
	if err != nil {
		return err
	}
	if r.Billing == "settled" || r.Billing == "released" {
		return nil
	}
	exists := func(query string) (bool, error) {
		var one int
		err := tx.QueryRowContext(ctx, query, p.Workspace, p.Environment, id).Scan(&one)
		if errors.Is(err, sql.ErrNoRows) {
			return false, nil
		}
		return err == nil, err
	}
	if pending && !r.OutputStarted {
		has, err := exists("SELECT 1 FROM sr_attempts WHERE workspace=$1 AND environment=$2 AND request=$3 LIMIT 1")
		if err != nil {
			return err
		}
		if !has {
			pending, execution = false, "cancelled"
		}
	}
	if usage != nil {
		over, err := exists("SELECT 1 FROM sr_attempts WHERE workspace=$1 AND environment=$2 AND request=$3 AND cost>cost_bound LIMIT 1")
		if err != nil {
			return err
		}
		if over {
			pending = true
		}
	}
	due := make([]*big.Int, len(r.Legs))
	exceeded := false
	for i, leg := range r.Legs {
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
		if r.Billing != "pending" {
			if err := s.event(ctx, tx, p, id, "reconciliation.required"); err != nil {
				return err
			}
		}
		if _, err := tx.ExecContext(ctx, "UPDATE sr_requests SET execution=$4,billing='pending' WHERE workspace=$1 AND environment=$2 AND id=$3", p.Workspace, p.Environment, id, execution); err != nil {
			return err
		}
		reason := "reservation_exceeded"
		if pending {
			reason = "unknown_outcome"
		}
		_, err := tx.ExecContext(ctx, "INSERT INTO sr_reconciliation(workspace,environment,request,reason) VALUES($1,$2,$3,$4) ON CONFLICT DO NOTHING", p.Workspace, p.Environment, id, reason)
		return err
	}
	for i, leg := range r.Legs {
		remaining := new(big.Int).Set(due[i])
		amount := due[i].String()
		rows, err := tx.QueryContext(ctx,
			"SELECT a.bucket,a.amount::text,to_json(a.windows)::text FROM sr_allocations a JOIN sr_buckets b ON (a.workspace,a.environment,a.bucket)=(b.workspace,b.environment,b.id) WHERE a.workspace=$1 AND a.environment=$2 AND a.request=$3 AND a.account=$4 ORDER BY b.expires_at ASC NULLS LAST, CASE b.source_kind WHEN 'subscription' THEN 0 WHEN 'promotion' THEN 1 ELSE 2 END,b.id",
			p.Workspace, p.Environment, id, leg.Account)
		if err != nil {
			return err
		}
		type allocation struct {
			bucket, amount string
			windows        []byte
		}
		var allocations []allocation
		for rows.Next() {
			var a allocation
			if err := rows.Scan(&a.bucket, &a.amount, &a.windows); err != nil {
				rows.Close()
				return err
			}
			allocations = append(allocations, a)
		}
		if err := rows.Close(); err != nil {
			return err
		}
		for _, a := range allocations {
			held := atoms(a.amount)
			take := new(big.Int).Set(remaining)
			if held.Cmp(take) < 0 {
				take = new(big.Int).Set(held)
			}
			remaining.Sub(remaining, take)
			if _, err := tx.ExecContext(ctx, "UPDATE sr_allocations SET charged=$5::numeric WHERE workspace=$1 AND environment=$2 AND request=$3 AND bucket=$4", p.Workspace, p.Environment, id, a.bucket, take.String()); err != nil {
				return err
			}
			if _, err := tx.ExecContext(ctx, "UPDATE sr_buckets SET remaining=remaining-$4::numeric,held=held-$5::numeric WHERE workspace=$1 AND environment=$2 AND id=$3", p.Workspace, p.Environment, a.bucket, take.String(), a.amount); err != nil {
				return err
			}
			if err := settleWindows(ctx, tx, p, jsonStrings(a.windows), a.amount, take.String()); err != nil {
				return err
			}
		}
		if _, err := tx.ExecContext(ctx, "UPDATE sr_accounts SET balance=balance-$4::numeric,held=held-$5::numeric WHERE workspace=$1 AND environment=$2 AND id=$3", p.Workspace, p.Environment, leg.Account, amount, leg.Reserved); err != nil {
			return err
		}
		if _, err := tx.ExecContext(ctx, "INSERT INTO sr_settlements VALUES($1,$2,$3,$4,$5,$6::numeric)", p.Workspace, p.Environment, id, leg.Kind, leg.Account, amount); err != nil {
			return err
		}
		if _, err := tx.ExecContext(ctx, "INSERT INTO sr_journal(workspace,environment,business_key,asset,debit,credit,amount,reason) VALUES($1,$2,$3,$4,$5,$6,$7::numeric,$8)",
			p.Workspace, p.Environment, "settle:"+id+":"+leg.Kind, leg.Price.Asset, leg.Account, "consumed:"+leg.Price.Asset, amount, "settlement"); err != nil {
			return err
		}
	}
	billing, kind := "released", "billing.released"
	if usage != nil {
		billing, kind = "settled", "billing.settled"
	}
	if _, err := tx.ExecContext(ctx, "UPDATE sr_requests SET execution=$4,billing=$5 WHERE workspace=$1 AND environment=$2 AND id=$3", p.Workspace, p.Environment, id, execution, billing); err != nil {
		return err
	}
	if err := s.expire(ctx, tx, p); err != nil {
		return err
	}
	return s.event(ctx, tx, p, id, kind)
}

// expire moves the unheld remainder of expired buckets out of the balance.
func (s *Store) expire(ctx context.Context, tx *sql.Tx, p contracts.Principal) error {
	rows, err := tx.QueryContext(ctx, "SELECT b.id,b.account,a.asset,(b.remaining-b.held)::text FROM sr_buckets b JOIN sr_accounts a ON (a.workspace,a.environment,a.id)=(b.workspace,b.environment,b.account) WHERE b.workspace=$1 AND b.environment=$2 AND b.expires_at<=now() AND b.remaining>b.held ORDER BY b.id FOR UPDATE OF b",
		p.Workspace, p.Environment)
	if err != nil {
		return err
	}
	type expired struct{ id, account, asset, amount string }
	var list []expired
	for rows.Next() {
		var e expired
		if err := rows.Scan(&e.id, &e.account, &e.asset, &e.amount); err != nil {
			rows.Close()
			return err
		}
		list = append(list, e)
	}
	if err := rows.Close(); err != nil {
		return err
	}
	for _, e := range list {
		if _, err := tx.ExecContext(ctx, "UPDATE sr_buckets SET remaining=held WHERE workspace=$1 AND environment=$2 AND id=$3", p.Workspace, p.Environment, e.id); err != nil {
			return err
		}
		if _, err := tx.ExecContext(ctx, "UPDATE sr_accounts SET balance=balance-$4::numeric WHERE workspace=$1 AND environment=$2 AND id=$3", p.Workspace, p.Environment, e.account, e.amount); err != nil {
			return err
		}
		if _, err := tx.ExecContext(ctx, "INSERT INTO sr_journal(workspace,environment,business_key,asset,debit,credit,amount,reason) VALUES($1,$2,$3,$4,$5,$6,$7::numeric,$8)",
			p.Workspace, p.Environment, "expire:"+uuid.New(), e.asset, e.account, "expired:"+e.asset, e.amount, "expiry"); err != nil {
			return err
		}
	}
	return nil
}

// Balance implements router.DataStore.
func (s *Store) Balance(ctx context.Context, p contracts.Principal) ([]router.BalanceRow, error) {
	rows := []router.BalanceRow{}
	err := s.tx(ctx, p, func(tx *sql.Tx) error {
		if err := s.expire(ctx, tx, p); err != nil {
			return err
		}
		r, err := tx.QueryContext(ctx,
			"SELECT id,kind,asset,balance::text,held::text,(balance-held)::text FROM sr_accounts WHERE workspace=$1 AND environment=$2 AND (owner=$3 OR (owner=$1 AND kind='platform_sale')) ORDER BY kind,id",
			p.Workspace, p.Environment, p.Customer)
		if err != nil {
			return err
		}
		defer r.Close()
		for r.Next() {
			var row router.BalanceRow
			if err := r.Scan(&row.ID, &row.Kind, &row.Asset, &row.Balance, &row.Held, &row.Available); err != nil {
				return err
			}
			rows = append(rows, row)
		}
		return r.Err()
	})
	return rows, err
}

// Cancel implements router.Store.
func (s *Store) Cancel(ctx context.Context, p contracts.Principal, id string) error {
	return s.tx(ctx, p, func(tx *sql.Tx) error {
		if _, err := s.row(ctx, tx, p, id); err != nil {
			return err
		}
		_, err := tx.ExecContext(ctx, "UPDATE sr_requests SET cancel_requested=true WHERE workspace=$1 AND environment=$2 AND id=$3", p.Workspace, p.Environment, id)
		return err
	})
}

// Disable implements router.DataStore.
func (s *Store) Disable(ctx context.Context, p contracts.Principal, offering string) error {
	if !p.HasScope("service:write") {
		return fail("scope_denied", 403)
	}
	_, err := s.db.ExecContext(ctx, "INSERT INTO sr_disabled_connections VALUES($1,$2,$3) ON CONFLICT DO NOTHING", p.Workspace, p.Environment, offering)
	return err
}

// Recover implements router.DataStore. It never redispatches an expired uncertain attempt.
func (s *Store) Recover(ctx context.Context) (int, error) {
	rows, err := s.db.QueryContext(ctx, "SELECT workspace,environment,customer,id FROM sr_requests WHERE deadline<now() AND billing='reserved' ORDER BY deadline,id")
	if err != nil {
		return 0, err
	}
	type target struct{ workspace, environment, customer, id string }
	var targets []target
	for rows.Next() {
		var t target
		if err := rows.Scan(&t.workspace, &t.environment, &t.customer, &t.id); err != nil {
			rows.Close()
			return 0, err
		}
		targets = append(targets, t)
	}
	if err := rows.Close(); err != nil {
		return 0, err
	}
	for _, t := range targets {
		p := contracts.Principal{Workspace: t.workspace, Environment: t.environment, Customer: t.customer}
		err := s.tx(ctx, p, func(tx *sql.Tx) error {
			r, err := s.row(ctx, tx, p, t.id)
			if err != nil {
				return err
			}
			if r.Billing != "reserved" {
				return nil
			}
			attempts, err := tx.QueryContext(ctx, "SELECT status,usage FROM sr_attempts WHERE workspace=$1 AND environment=$2 AND request=$3 ORDER BY created_at,id", p.Workspace, p.Environment, t.id)
			if err != nil {
				return err
			}
			var (
				count      int
				lastStatus string
				lastUsage  []byte
			)
			for attempts.Next() {
				count++
				lastUsage = nil
				if err := attempts.Scan(&lastStatus, &lastUsage); err != nil {
					attempts.Close()
					return err
				}
			}
			if err := attempts.Close(); err != nil {
				return err
			}
			known := count > 0 && (lastStatus == "succeeded" || lastStatus == "partial") && len(lastUsage) > 0 && string(lastUsage) != "null"
			execution, usage := "cancelled", (*contracts.Meters)(nil)
			switch {
			case known:
				usage = &contracts.Meters{}
				if err := json.Unmarshal(lastUsage, usage); err != nil {
					return err
				}
				execution = lastStatus
			case count > 0:
				execution = "unknown"
			}
			return s.finishTx(ctx, tx, p, t.id, execution, usage, count > 0 && !known)
		})
		if err != nil {
			return 0, err
		}
	}
	return len(targets), nil
}

// Entitle lets a customer use subscription-only services for a period. Control planes do this together with a
// subscription payment; this helper is for self-hosts and tests. bucket must be an existing bucket id.
func (s *Store) Entitle(ctx context.Context, p contracts.Principal, subscription string, cycle int, serviceIDs []string, start, end time.Time, bucket string) error {
	return s.EntitleWith(ctx, p, subscription, cycle, serviceIDs, start, end, bucket, nil)
}

// EntitleWith is Entitle with the paid cycle's window limits snapshot (the plan's window_limits object with
// enabled:true), or nil when the option is off. The control plane writes this row in the TypeScript store.
func (s *Store) EntitleWith(ctx context.Context, p contracts.Principal, subscription string, cycle int, serviceIDs []string, start, end time.Time, bucket string, windowLimits json.RawMessage) error {
	var limits any
	if len(windowLimits) > 0 {
		limits = string(windowLimits)
	}
	_, err := s.db.ExecContext(ctx,
		"INSERT INTO sr_entitlements(workspace,environment,subscription,cycle,customer,service_ids,period_start,period_end,bucket,window_limits) VALUES($1,$2,$3,$4,$5,"+fmt.Sprintf(arrayExpr, "$6")+",$7,$8,$9,$10::jsonb)",
		p.Workspace, p.Environment, subscription, cycle, p.Customer, arrayArg(serviceIDs), start, end, bucket, limits)
	return err
}

// Buckets lists a tenant's funding buckets ordered by source. For tests and diagnostics.
func (s *Store) Buckets(ctx context.Context, p contracts.Principal) ([]BucketRow, error) {
	rows, err := s.db.QueryContext(ctx, "SELECT source,remaining::text,held::text FROM sr_buckets WHERE workspace=$1 AND environment=$2 ORDER BY source", p.Workspace, p.Environment)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var out []BucketRow
	for rows.Next() {
		var b BucketRow
		if err := rows.Scan(&b.Source, &b.Remaining, &b.Held); err != nil {
			return nil, err
		}
		out = append(out, b)
	}
	return out, rows.Err()
}

// BucketRow is a funding bucket as reported by Buckets.
type BucketRow struct {
	Source    string `json:"source"`
	Remaining string `json:"remaining"`
	Held      string `json:"held"`
}
