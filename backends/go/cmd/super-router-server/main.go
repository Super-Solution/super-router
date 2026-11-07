// SPDX-License-Identifier: Apache-2.0

// Command super-router-server runs the Super Router data plane: the HTTP API, a storage backend and the
// configured providers. It is configured through environment variables:
//
//	SUPERROUTER_API_KEY      "sr_" plus 64 hexadecimal characters; the bearer token of the single owner key.
//	                         Required unless SUPERROUTER_AUTH=database
//	SUPERROUTER_DIGEST_KEY   required, at least 32 characters; keys the idempotency digest
//	SUPERROUTER_AUTH         "database" authenticates bearer tokens against the control plane's keys in
//	                         PostgreSQL (members, scoped keys, revocation) instead of the single owner key;
//	                         needs DATABASE_URL. Keys are created by the TypeScript control plane
//	DATABASE_URL             PostgreSQL URL. Unset means the in-memory store: state is lost on restart
//	ADDR                     listen address, default 127.0.0.1:3410
//	SUPERROUTER_WORKSPACE, SUPERROUTER_ENVIRONMENT, SUPERROUTER_CUSTOMER
//	                         tenant of the owner key, default default, prod, default
//	OPENAI_API_KEY, ANTHROPIC_API_KEY
//	                         enable the "openai" and "anthropic" providers (offering.provider names them)
//	SUPERROUTER_MOCK=1       enable the deterministic "mock" provider
package main

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"log/slog"
	"net"
	"net/http"
	"os"
	"os/signal"
	"regexp"
	"syscall"
	"time"

	_ "github.com/jackc/pgx/v5/stdlib"

	"github.com/Super-Solution/super-router/backends/go/contracts"
	"github.com/Super-Solution/super-router/backends/go/provider/anthropic"
	"github.com/Super-Solution/super-router/backends/go/provider/openai"
	"github.com/Super-Solution/super-router/backends/go/router"
	"github.com/Super-Solution/super-router/backends/go/server"
	"github.com/Super-Solution/super-router/backends/go/store/memory"
	"github.com/Super-Solution/super-router/backends/go/store/postgres"
)

var apiKeyRe = regexp.MustCompile(`^sr_[a-f0-9]{64}$`)

func main() {
	logger := slog.New(slog.NewJSONHandler(os.Stderr, nil))
	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()
	if err := run(ctx, os.Getenv, logger, nil); err != nil {
		logger.Error("server stopped", "error", err)
		os.Exit(1)
	}
}

// run starts the server and blocks until ctx is cancelled. ready, when set, receives the bound address.
func run(ctx context.Context, getenv func(string) string, logger *slog.Logger, ready chan<- string) error {
	apiKey, digestKey := getenv("SUPERROUTER_API_KEY"), getenv("SUPERROUTER_DIGEST_KEY")
	databaseAuth := getenv("SUPERROUTER_AUTH") == "database"
	if getenv("SUPERROUTER_AUTH") != "" && !databaseAuth {
		return errors.New("SUPERROUTER_AUTH must be empty or \"database\"")
	}
	if databaseAuth && getenv("DATABASE_URL") == "" {
		return errors.New("SUPERROUTER_AUTH=database needs DATABASE_URL")
	}
	if (!databaseAuth && !apiKeyRe.MatchString(apiKey)) || len(digestKey) < 32 {
		return errors.New("set SUPERROUTER_API_KEY (sr_ plus 64 random hexadecimal characters; not needed with SUPERROUTER_AUTH=database) and SUPERROUTER_DIGEST_KEY (at least 32 characters); values are not logged")
	}
	or := func(name, fallback string) string {
		if v := getenv(name); v != "" {
			return v
		}
		return fallback
	}
	var store router.DataStore
	var auth server.Authenticator
	if url := getenv("DATABASE_URL"); url != "" {
		db, err := sql.Open("pgx", url)
		if err != nil {
			return err
		}
		defer db.Close()
		db.SetMaxOpenConns(20)
		opts := postgres.Options{}
		if databaseAuth {
			opts.Revalidate = postgres.Revalidate
		}
		pg := postgres.New(db, opts)
		if err := pg.Migrate(ctx); err != nil {
			return fmt.Errorf("migrate: %w", err)
		}
		store = pg
		if databaseAuth {
			auth = postgres.NewKeys(db)
		}
	} else {
		logger.Warn("DATABASE_URL is not set: using the in-memory store, state is lost on restart")
		store = memory.New(nil)
	}
	providers := map[string]contracts.Provider{}
	if k := getenv("OPENAI_API_KEY"); k != "" {
		providers["openai"] = openai.New(func() string { return k })
	}
	if k := getenv("ANTHROPIC_API_KEY"); k != "" {
		providers["anthropic"] = anthropic.New(func() string { return k })
	}
	if getenv("SUPERROUTER_MOCK") == "1" {
		providers["mock"] = router.NewMockProvider("success")
	}
	if len(providers) == 0 {
		logger.Warn("no provider is configured: set OPENAI_API_KEY, ANTHROPIC_API_KEY or SUPERROUTER_MOCK=1")
	}
	rt, err := router.New(store, providers, digestKey)
	if err != nil {
		return err
	}
	owner := contracts.Principal{
		Workspace: or("SUPERROUTER_WORKSPACE", "default"), Environment: or("SUPERROUTER_ENVIRONMENT", "prod"), Customer: or("SUPERROUTER_CUSTOMER", "default"),
		Scopes: []string{"invoke", "service:write", "billing:write", "billing:read", "operations:read", "routing:write"},
	}
	if auth == nil {
		auth = server.StaticKeys([]server.Key{{Secret: apiKey, Principal: owner}})
	}
	api, err := server.Handler(server.Options{Router: rt, Store: store, Logger: logger, Auth: auth})
	if err != nil {
		return err
	}
	listener, err := net.Listen("tcp", or("ADDR", "127.0.0.1:3410"))
	if err != nil {
		return err
	}
	srv := &http.Server{
		Handler: api, ReadHeaderTimeout: 10 * time.Second, ReadTimeout: time.Minute, IdleTimeout: 2 * time.Minute,
		// No WriteTimeout: streaming responses legitimately stay open.
	}
	go recoverLoop(ctx, store, logger)
	logger.Info("listening", "addr", listener.Addr().String(), "store", fmt.Sprintf("%T", store), "providers", len(providers))
	if ready != nil {
		ready <- listener.Addr().String()
	}
	failed := make(chan error, 1)
	go func() { failed <- srv.Serve(listener) }()
	select {
	case err := <-failed:
		return err
	case <-ctx.Done():
	}
	shutdown, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	return srv.Shutdown(shutdown)
}

// recoverLoop settles or releases requests whose deadline passed, for example after a crash mid-call.
func recoverLoop(ctx context.Context, store router.DataStore, logger *slog.Logger) {
	tick := time.NewTicker(30 * time.Second)
	defer tick.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-tick.C:
			if n, err := store.Recover(ctx); err != nil {
				logger.Error("recover failed", "error", err)
			} else if n > 0 {
				logger.Info("recovered expired requests", "count", n)
			}
		}
	}
}
