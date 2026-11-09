// SPDX-License-Identifier: Apache-2.0

package postgres_test

import (
	"context"
	"database/sql"
	"os"
	"path/filepath"
	"testing"
	"time"

	_ "github.com/jackc/pgx/v5/stdlib"

	"github.com/Super-Solution/super-router/backends/go/contracts"
	"github.com/Super-Solution/super-router/backends/go/internal/dataplanetest"
	"github.com/Super-Solution/super-router/backends/go/store/postgres"
)

const localURL = "postgresql://superrouter:local-development-only@127.0.0.1:25432/superrouter"

// open connects to DATABASE_URL (or the local development database) and skips when it is not reachable.
// Set SR_REQUIRE_POSTGRES=1 to turn that skip into a failure, as CI should.
func open(t *testing.T) *postgres.Store {
	t.Helper()
	url := os.Getenv("DATABASE_URL")
	if url == "" {
		url = localURL
	}
	db, err := sql.Open("pgx", url)
	if err == nil {
		ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
		defer cancel()
		err = db.PingContext(ctx)
	}
	if err != nil {
		if os.Getenv("SR_REQUIRE_POSTGRES") != "" {
			t.Fatalf("postgres required but unreachable: %v", err)
		}
		t.Skipf("postgres unreachable (set DATABASE_URL): %v", err)
	}
	t.Cleanup(func() { db.Close() })
	store := postgres.New(db, postgres.Options{})
	if err := store.Migrate(context.Background()); err != nil {
		t.Fatal(err)
	}
	return store
}

func TestDataplaneConformance(t *testing.T) {
	store := open(t)
	dataplanetest.Run(t, "postgres", func(t *testing.T) *dataplanetest.Harness {
		// Recovery is global: settle what earlier runs left expired so a scenario counts only its own.
		if _, err := store.Recover(context.Background()); err != nil {
			t.Fatal(err)
		}
		return &dataplanetest.Harness{
			Store:   store,
			Clock:   time.Now,
			Advance: func(ms int64) { time.Sleep(time.Duration(ms) * time.Millisecond) },
			Buckets: func(p contracts.Principal) []dataplanetest.BucketRow {
				rows, err := store.Buckets(context.Background(), p)
				if err != nil {
					t.Fatal(err)
				}
				out := make([]dataplanetest.BucketRow, 0, len(rows))
				for _, r := range rows {
					out = append(out, dataplanetest.BucketRow{Source: r.Source, Remaining: r.Remaining, Held: r.Held})
				}
				return out
			},
		}
	})
}

// The embedded migrations must stay byte-identical to the TypeScript store's, which share the schema.
func TestMigrationsMatchTypeScript(t *testing.T) {
	theirs, err := filepath.Glob(filepath.Join("..", "..", "..", "..", "packages", "adapters", "storage-postgres", "migrations", "*.sql"))
	if err != nil || len(theirs) == 0 {
		t.Skip("TypeScript migrations not found next to this module")
	}
	ours, _ := filepath.Glob(filepath.Join("migrations", "*.sql"))
	if len(ours) != len(theirs) {
		t.Fatalf("%d embedded migrations, %d in the TypeScript store", len(ours), len(theirs))
	}
	for i := range theirs {
		a, _ := os.ReadFile(theirs[i])
		b, _ := os.ReadFile(ours[i])
		if string(a) != string(b) || filepath.Base(theirs[i]) != filepath.Base(ours[i]) {
			t.Errorf("%s differs from the TypeScript store", filepath.Base(ours[i]))
		}
	}
}

func TestMigrateIsIdempotent(t *testing.T) {
	store := open(t)
	if err := store.Migrate(context.Background()); err != nil {
		t.Fatal(err)
	}
}

func TestConcurrency(t *testing.T) {
	store := open(t)
	dataplanetest.Concurrency(t, &dataplanetest.Harness{Store: store, Clock: time.Now})
}
