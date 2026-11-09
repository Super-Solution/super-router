// SPDX-License-Identifier: Apache-2.0

package postgres

import (
	"context"
	"crypto/sha256"
	"database/sql"
	"encoding/hex"
	"encoding/json"
	"errors"
	"regexp"
	"slices"

	"github.com/Super-Solution/super-router/backends/go/contracts"
)

var tokenRe = regexp.MustCompile(`^sr_[a-f0-9]{64}$`)

// Keys authenticates bearer tokens against the control plane's key table (sr_keys, sr_members), which the
// TypeScript control plane writes. Only the SHA-256 of a token is stored; revoked, expired and deactivated-member
// keys are refused. It implements server.Authenticator.
type Keys struct{ db *sql.DB }

// NewKeys returns an authenticator reading keys through db.
func NewKeys(db *sql.DB) *Keys { return &Keys{db: db} }

func hashToken(token string) string {
	sum := sha256.Sum256([]byte(token))
	return hex.EncodeToString(sum[:])
}

// Authenticate turns a token into the principal its key grants: the key's scopes narrowed to what the member's
// role still allows.
func (k *Keys) Authenticate(ctx context.Context, token string) (contracts.Principal, error) {
	if !tokenRe.MatchString(token) {
		return contracts.Principal{}, fail("invalid_key", 401)
	}
	return principalByHash(ctx, k.db, hashToken(token))
}

type queryer interface {
	QueryRowContext(ctx context.Context, query string, args ...any) *sql.Row
}

func principalByHash(ctx context.Context, db queryer, hash string) (contracts.Principal, error) {
	var (
		p            contracts.Principal
		role, scopes string
		serviceIDs   sql.NullString
		active       bool
	)
	err := db.QueryRowContext(ctx,
		"SELECT k.workspace,k.environment,k.customer,k.id,k.member_id,to_json(k.scopes)::text,to_json(k.service_ids)::text,m.role,m.active"+
			" FROM sr_keys k JOIN sr_members m ON (k.workspace,k.environment,k.member_id)=(m.workspace,m.environment,m.id)"+
			" WHERE k.hash=$1 AND k.revoked_at IS NULL AND k.expires_at>now()", hash).
		Scan(&p.Workspace, &p.Environment, &p.Customer, &p.AuthKeyID, &p.MemberID, &scopes, &serviceIDs, &role, &active)
	if errors.Is(err, sql.ErrNoRows) || (err == nil && !active) {
		return contracts.Principal{}, fail("invalid_key", 401)
	}
	if err != nil {
		return contracts.Principal{}, err
	}
	var granted []string
	if err := json.Unmarshal([]byte(scopes), &granted); err != nil {
		return contracts.Principal{}, err
	}
	allowed := contracts.RoleScopes[role]
	p.Scopes = []string{}
	for _, s := range granted {
		if slices.Contains(allowed, s) {
			p.Scopes = append(p.Scopes, s)
		}
	}
	if serviceIDs.Valid && serviceIDs.String != "null" {
		if err := json.Unmarshal([]byte(serviceIDs.String), &p.ServiceIDs); err != nil {
			return contracts.Principal{}, err
		}
	}
	return p, nil
}

// Revalidate is for Options.Revalidate: inside a write transaction it re-reads the principal's key and refuses the
// write if the key was revoked, expired, moved to another customer or narrowed since it was verified. Principals
// that did not come from a key (AuthKeyID empty) pass.
func Revalidate(ctx context.Context, tx *sql.Tx, p contracts.Principal, _ string) error {
	if p.AuthKeyID == "" {
		return nil
	}
	var hash string
	err := tx.QueryRowContext(ctx, "SELECT hash FROM sr_keys WHERE workspace=$1 AND environment=$2 AND id=$3",
		p.Workspace, p.Environment, p.AuthKeyID).Scan(&hash)
	if errors.Is(err, sql.ErrNoRows) {
		return fail("invalid_key", 401)
	}
	if err != nil {
		return err
	}
	current, err := principalByHash(ctx, tx, hash)
	if err != nil {
		return err
	}
	if current.Customer != p.Customer {
		return fail("scope_denied", 403)
	}
	for _, s := range p.Scopes {
		if !current.HasScope(s) {
			return fail("scope_denied", 403)
		}
	}
	if current.ServiceIDs != nil {
		if p.ServiceIDs == nil {
			return fail("scope_denied", 403)
		}
		for _, s := range p.ServiceIDs {
			if !slices.Contains(current.ServiceIDs, s) {
				return fail("scope_denied", 403)
			}
		}
	}
	return nil
}
