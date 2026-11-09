// SPDX-License-Identifier: Apache-2.0

package main

import (
	"context"
	"database/sql"
	"io"
	"log/slog"
	"net/http"
	"os"
	"strings"
	"testing"

	_ "github.com/jackc/pgx/v5/stdlib"
	"time"
)

const apiKey = "sr_0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"

func TestRefusesWeakConfiguration(t *testing.T) {
	for _, env := range []map[string]string{{}, {"SUPERROUTER_API_KEY": "short", "SUPERROUTER_DIGEST_KEY": strings.Repeat("d", 32)}, {"SUPERROUTER_API_KEY": apiKey, "SUPERROUTER_DIGEST_KEY": "short"}} {
		err := run(context.Background(), func(k string) string { return env[k] }, slog.New(slog.NewTextHandler(io.Discard, nil)), nil)
		if err == nil {
			t.Fatalf("accepted %v", env)
		}
	}
}

func TestServesThroughTheMemoryStoreAndShutsDownCleanly(t *testing.T) {
	env := map[string]string{"SUPERROUTER_API_KEY": apiKey, "SUPERROUTER_DIGEST_KEY": strings.Repeat("d", 32), "ADDR": "127.0.0.1:0", "SUPERROUTER_MOCK": "1"}
	ctx, stop := context.WithCancel(context.Background())
	ready, done := make(chan string, 1), make(chan error, 1)
	go func() {
		done <- run(ctx, func(k string) string { return env[k] }, slog.New(slog.NewTextHandler(io.Discard, nil)), ready)
	}()
	addr := <-ready
	base := "http://" + addr
	do := func(method, path, body string) (*http.Response, string) {
		req, _ := http.NewRequest(method, base+path, strings.NewReader(body))
		req.Header.Set("Authorization", "Bearer "+apiKey)
		req.Header.Set("Idempotency-Key", "k-"+path)
		res, err := http.DefaultClient.Do(req)
		if err != nil {
			t.Fatal(err)
		}
		defer res.Body.Close()
		raw, _ := io.ReadAll(res.Body)
		return res, string(raw)
	}
	if res, _ := do("GET", "/healthz", ""); res.StatusCode != 200 {
		t.Fatalf("healthz %d", res.StatusCode)
	}
	if res, _ := do("GET", "/v1/services", ""); res.StatusCode != 200 {
		t.Fatalf("services %d", res.StatusCode)
	}
	if res, body := do("GET", "/v1/requests/x", ""); res.StatusCode != 404 || !strings.Contains(body, "resource_not_found") {
		t.Fatalf("%d %s", res.StatusCode, body)
	}
	stop()
	select {
	case err := <-done:
		if err != nil && err != http.ErrServerClosed {
			t.Fatal(err)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("server did not shut down")
	}
}

func TestRefusesBadAuthConfiguration(t *testing.T) {
	digest := strings.Repeat("d", 32)
	for _, env := range []map[string]string{
		{"SUPERROUTER_AUTH": "database", "SUPERROUTER_DIGEST_KEY": digest}, // needs a database
		{"SUPERROUTER_AUTH": "magic", "SUPERROUTER_API_KEY": apiKey, "SUPERROUTER_DIGEST_KEY": digest},
	} {
		err := run(context.Background(), func(k string) string { return env[k] }, slog.New(slog.NewTextHandler(io.Discard, nil)), nil)
		if err == nil {
			t.Fatalf("accepted %v", env)
		}
	}
}

func TestDatabaseAuthRejectsUnknownKeys(t *testing.T) {
	url := os.Getenv("DATABASE_URL")
	if url == "" {
		url = "postgresql://superrouter:local-development-only@127.0.0.1:25432/superrouter"
	}
	db, err := sql.Open("pgx", url)
	if err == nil {
		err = db.Ping()
	}
	if err != nil {
		if os.Getenv("SR_REQUIRE_POSTGRES") != "" {
			t.Fatalf("postgres required but unreachable: %v", err)
		}
		t.Skipf("postgres unreachable: %v", err)
	}
	db.Close()
	env := map[string]string{"SUPERROUTER_AUTH": "database", "DATABASE_URL": url, "SUPERROUTER_DIGEST_KEY": strings.Repeat("d", 32), "ADDR": "127.0.0.1:0"}
	ctx, stop := context.WithCancel(context.Background())
	ready, done := make(chan string, 1), make(chan error, 1)
	go func() {
		done <- run(ctx, func(k string) string { return env[k] }, slog.New(slog.NewTextHandler(io.Discard, nil)), ready)
	}()
	base := "http://" + <-ready
	req, _ := http.NewRequest("GET", base+"/v1/services", nil)
	req.Header.Set("Authorization", "Bearer "+apiKey) // well-formed, but no such key in the database
	res, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	res.Body.Close()
	if res.StatusCode != 401 {
		t.Fatalf("status %d", res.StatusCode)
	}
	stop()
	<-done
}
