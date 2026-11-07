// SPDX-License-Identifier: Apache-2.0

package main

import (
	"context"
	"io"
	"log/slog"
	"net/http"
	"strings"
	"testing"
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
