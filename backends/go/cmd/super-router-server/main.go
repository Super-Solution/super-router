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
//	OPENAI_API_KEY, ANTHROPIC_API_KEY, GEMINI_API_KEY
//	                         enable the "openai", "anthropic" and "gemini" providers (offering.provider names them)
//	OPENAI_BASE_URL, ANTHROPIC_BASE_URL, GEMINI_BASE_URL
//	                         where that provider's requests go: the scheme, host and any path prefix, without the
//	                         trailing /v1 (or /v1beta) the provider adds itself, so https://llm.internal/openai becomes
//	                         https://llm.internal/openai/v1/responses. Reaches Azure OpenAI in its v1 form, a gateway
//	                         or any compatible host. Default: the vendor's own host
//	SUPERROUTER_HEALTH       "on" keeps an offering out of routing while its own failure repeats (rejected key, empty
//	                         account, retired model, blocked region, rate limit) and probes it again with one request
//	                         when its cooldown ends; every candidate cooling fails open. "off" or unset: nothing is skipped
//	SUPERROUTER_HEALTH_RATE_LIMITED_MS, _QUOTA_EXHAUSTED_MS, _CREDENTIALS_REJECTED_MS, _MODEL_UNAVAILABLE_MS,
//	_REGION_BLOCKED_MS, _MAX_MS, _PROBE_LEASE_MS
//	                         the cooldown of each class, the ceiling it doubles up to and the probe lease, in
//	                         milliseconds (1 to 86400000). Defaults: 5000, 300000, 300000, 600000, 600000, 1800000,
//	                         30000. Only valid with SUPERROUTER_HEALTH=on
//	SUPERROUTER_MOCK=1       enable the deterministic "mock" provider and the scripted "mock-tools" one
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
	"github.com/Super-Solution/super-router/backends/go/provider/gemini"
	"github.com/Super-Solution/super-router/backends/go/provider/openai"
	"github.com/Super-Solution/super-router/backends/go/router"
	"github.com/Super-Solution/super-router/backends/go/routing"
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
	var bases [3]string
	for i, name := range []string{"OPENAI_BASE_URL", "ANTHROPIC_BASE_URL", "GEMINI_BASE_URL"} {
		if v := getenv(name); v != "" {
			b, err := baseURL(name, v)
			if err != nil {
				return err
			}
			bases[i] = b
		}
	}
	health, err := healthPolicy(getenv)
	if err != nil {
		return err
	}
	providers := map[string]contracts.Provider{}
	if k := getenv("OPENAI_API_KEY"); k != "" {
		var opts []openai.Option
		if bases[0] != "" {
			opts = append(opts, openai.WithBaseURL(bases[0]))
		}
		providers["openai"] = openai.New(func() string { return k }, opts...)
	}
	if k := getenv("ANTHROPIC_API_KEY"); k != "" {
		var opts []anthropic.Option
		if bases[1] != "" {
			opts = append(opts, anthropic.WithBaseURL(bases[1]))
		}
		providers["anthropic"] = anthropic.New(func() string { return k }, opts...)
	}
	if k := getenv("GEMINI_API_KEY"); k != "" {
		var opts []gemini.Option
		if bases[2] != "" {
			opts = append(opts, gemini.WithBaseURL(bases[2]))
		}
		providers["gemini"] = gemini.New(func() string { return k }, opts...)
	}
	if getenv("SUPERROUTER_MOCK") == "1" {
		providers["mock"] = router.NewMockProvider("success")
		// Scripted tool calling, so a client's tool loop can be exercised without a model key.
		providers["mock-tools"] = &router.MockToolProvider{}
	}
	if len(providers) == 0 {
		logger.Warn("no provider is configured: set OPENAI_API_KEY, ANTHROPIC_API_KEY, GEMINI_API_KEY or SUPERROUTER_MOCK=1")
	}
	var routerOptions []router.Option
	if health != nil {
		routerOptions = append(routerOptions, router.WithHealth(routing.NewHealthBook(*health)))
	}
	rt, err := router.New(store, providers, digestKey, routerOptions...)
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
	logger.Info("listening", "addr", listener.Addr().String(), "store", fmt.Sprintf("%T", store), "providers", len(providers), "health", health != nil)
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
