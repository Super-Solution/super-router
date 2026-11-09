// SPDX-License-Identifier: Apache-2.0

package postgres_test

import (
	"context"
	"crypto/rand"
	"crypto/sha256"
	"database/sql"
	"encoding/hex"
	"errors"
	"testing"

	"github.com/Super-Solution/super-router/backends/go/contracts"
	"github.com/Super-Solution/super-router/backends/go/internal/uuid"
	"github.com/Super-Solution/super-router/backends/go/store/postgres"
)

type seeded struct {
	db        *sql.DB
	ws        string
	keys      *postgres.Keys
	ownerKey  string
	ownerID   string
	appToken  string
	appKeyID  string
	viewerTok string
}

func token() string {
	b := make([]byte, 32)
	_, _ = rand.Read(b)
	return "sr_" + hex.EncodeToString(b)
}

func seed(t *testing.T) *seeded {
	t.Helper()
	store := open(t)
	db := store.DB()
	ctx := context.Background()
	s := &seeded{db: db, ws: "ws-" + uuid.New(), keys: postgres.NewKeys(db)}
	exec := func(q string, args ...any) {
		if _, err := db.ExecContext(ctx, q, args...); err != nil {
			t.Fatal(err)
		}
	}
	for _, m := range [][2]string{{"owner1", "owner"}, {"app1", "app"}, {"view1", "viewer"}} {
		exec("INSERT INTO sr_members(workspace,environment,id,role) VALUES($1,'test',$2,$3)", s.ws, m[0], m[1])
	}
	exec("INSERT INTO sr_customers(workspace,environment,id,external_id) VALUES($1,'test','c1','ext1')", s.ws)
	insert := func(member, customer string, scopes string, services any) (string, string) {
		tok, id := token(), uuid.New()
		sum := sha256.Sum256([]byte(tok))
		exec("INSERT INTO sr_keys(workspace,environment,id,member_id,customer,hash,scopes,service_ids,expires_at)"+
			" VALUES($1,'test',$2,$3,$4,$5,$6::text[],$7::text[],now()+interval '1 day')",
			s.ws, id, member, customer, hex.EncodeToString(sum[:]), scopes, services)
		return tok, id
	}
	s.ownerKey, s.ownerID = insert("owner1", "_admin", "{invoke,service:write,billing:write,identity:write}", nil)
	s.appToken, s.appKeyID = insert("app1", "c1", "{invoke,billing:write}", "{chat}") // billing:write is beyond the app role
	s.viewerTok, _ = insert("view1", "_admin", "{billing:read}", nil)
	return s
}

func TestKeysAuthenticate(t *testing.T) {
	s := seed(t)
	ctx := context.Background()
	p, err := s.keys.Authenticate(ctx, s.ownerKey)
	if err != nil || p.Workspace != s.ws || p.Customer != "_admin" || p.AuthKeyID != s.ownerID || p.MemberID != "owner1" || p.ServiceIDs != nil {
		t.Fatalf("%+v %v", p, err)
	}
	app, err := s.keys.Authenticate(ctx, s.appToken)
	if err != nil {
		t.Fatal(err)
	}
	// The key lists billing:write but the app role does not allow it, so it is dropped.
	if len(app.Scopes) != 1 || app.Scopes[0] != "invoke" || len(app.ServiceIDs) != 1 || app.ServiceIDs[0] != "chat" || app.Customer != "c1" {
		t.Fatalf("%+v", app)
	}
	for name, tok := range map[string]string{"unknown": token(), "malformed": "sr_nope", "empty": ""} {
		_, err := s.keys.Authenticate(ctx, tok)
		var re *contracts.RouterError
		if !errors.As(err, &re) || re.Code != "invalid_key" || re.Status != 401 {
			t.Errorf("%s: %v", name, err)
		}
	}
}

func TestKeysRefuseRevokedExpiredAndInactive(t *testing.T) {
	s := seed(t)
	ctx := context.Background()
	check := func(label string) {
		t.Helper()
		_, err := s.keys.Authenticate(ctx, s.ownerKey)
		var re *contracts.RouterError
		if !errors.As(err, &re) || re.Code != "invalid_key" {
			t.Fatalf("%s: %v", label, err)
		}
	}
	mustExec := func(q string, args ...any) {
		if _, err := s.db.ExecContext(ctx, q, args...); err != nil {
			t.Fatal(err)
		}
	}
	mustExec("UPDATE sr_members SET active=false WHERE workspace=$1 AND id='owner1'", s.ws)
	check("inactive member")
	mustExec("UPDATE sr_members SET active=true WHERE workspace=$1 AND id='owner1'", s.ws)
	if _, err := s.keys.Authenticate(ctx, s.ownerKey); err != nil {
		t.Fatal(err)
	}
	mustExec("UPDATE sr_keys SET expires_at=now()-interval '1 second' WHERE workspace=$1 AND id=$2", s.ws, s.ownerID)
	check("expired")
	mustExec("UPDATE sr_keys SET expires_at=now()+interval '1 day',revoked_at=now() WHERE workspace=$1 AND id=$2", s.ws, s.ownerID)
	check("revoked")
}

// A key that was fine when the request was authenticated must not write after it was revoked or narrowed.
func TestRevalidateStopsWritesAfterRevocation(t *testing.T) {
	s := seed(t)
	ctx := context.Background()
	store := postgres.New(s.db, postgres.Options{Revalidate: postgres.Revalidate})
	p, err := s.keys.Authenticate(ctx, s.ownerKey)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := store.Grant(ctx, p, "customer_sale", "credit:demo", "5", "g1", nil); err != nil {
		t.Fatal(err)
	}
	if _, err := s.db.ExecContext(ctx, "UPDATE sr_keys SET scopes='{invoke}' WHERE workspace=$1 AND id=$2", s.ws, s.ownerID); err != nil {
		t.Fatal(err)
	}
	_, err = store.Grant(ctx, p, "customer_sale", "credit:demo", "5", "g2", nil)
	var re *contracts.RouterError
	if !errors.As(err, &re) || re.Code != "scope_denied" {
		t.Fatalf("narrowed key wrote: %v", err)
	}
	if _, err := s.db.ExecContext(ctx, "UPDATE sr_keys SET revoked_at=now() WHERE workspace=$1 AND id=$2", s.ws, s.ownerID); err != nil {
		t.Fatal(err)
	}
	_, err = store.Grant(ctx, p, "customer_sale", "credit:demo", "5", "g3", nil)
	if !errors.As(err, &re) || re.Code != "invalid_key" {
		t.Fatalf("revoked key wrote: %v", err)
	}
	// A principal that did not come from a key is the host's own and is not revalidated.
	host := contracts.Principal{Workspace: s.ws, Environment: "test", Customer: "c1", Scopes: []string{"billing:write"}}
	if _, err := store.Grant(ctx, host, "customer_sale", "credit:demo", "5", "g4", nil); err != nil {
		t.Fatal(err)
	}
}
