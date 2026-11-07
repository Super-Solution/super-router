// SPDX-License-Identifier: Apache-2.0

// Package server is the HTTP API of the Super Router data plane as a plain net/http handler.
//
// Handler returns an http.Handler, so it mounts on the standard library mux and on every framework that
// accepts one: chi (r.Mount), gin (gin.WrapH), echo (echo.WrapHandler), fiber (adaptor.HTTPHandler) and so on.
// Authentication is the Authenticator port; storage is router.DataStore. Responses and error codes match the
// TypeScript reference server.
package server

import (
	"bytes"
	"context"
	"crypto/sha256"
	"crypto/subtle"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net/http"
	"strings"

	"github.com/Super-Solution/super-router/backends/go/contracts"
	"github.com/Super-Solution/super-router/backends/go/router"
	"github.com/Super-Solution/super-router/backends/go/routing"
)

// MaxBodyBytes caps every request body.
const MaxBodyBytes = 256_000

// Authenticator turns a bearer token into a principal. Return a *contracts.RouterError "invalid_key" (401)
// for an unknown or revoked key. Implement it to back keys with a database or a JWT verifier.
type Authenticator interface {
	Authenticate(ctx context.Context, token string) (contracts.Principal, error)
}

// Key is one static API key for self-hosting.
type Key struct {
	Secret    string
	Principal contracts.Principal
}

type staticKeys []struct {
	digest    [32]byte
	principal contracts.Principal
}

// StaticKeys authenticates against a fixed list of keys, comparing digests in constant time.
func StaticKeys(keys []Key) Authenticator {
	out := make(staticKeys, 0, len(keys))
	for _, k := range keys {
		out = append(out, struct {
			digest    [32]byte
			principal contracts.Principal
		}{sha256.Sum256([]byte(k.Secret)), k.Principal})
	}
	return out
}

func (s staticKeys) Authenticate(_ context.Context, token string) (contracts.Principal, error) {
	digest := sha256.Sum256([]byte(token))
	var found *contracts.Principal
	for i := range s {
		if subtle.ConstantTimeCompare(s[i].digest[:], digest[:]) == 1 {
			found = &s[i].principal
		}
	}
	if found == nil {
		return contracts.Principal{}, contracts.NewError("invalid_key", 401)
	}
	return *found, nil
}

// Options configures Handler.
type Options struct {
	Router *router.Router
	Store  router.DataStore
	Auth   Authenticator
	// Mode is "self-host" (default) or "cloud". Cloud mode forbids bring-your-own-key offerings and protects
	// platform pricing from API keys.
	Mode   string
	Logger *slog.Logger
}

type handler struct {
	Options
	log *slog.Logger
}

type ctxKey struct{}

// Handler returns the HTTP API. GET /healthz needs no credentials; everything else needs a bearer token.
func Handler(o Options) (http.Handler, error) {
	if o.Router == nil || o.Store == nil || o.Auth == nil {
		return nil, errors.New("server: Router, Store and Auth are required")
	}
	if o.Mode == "" {
		o.Mode = "self-host"
	}
	if o.Logger == nil {
		o.Logger = slog.Default()
	}
	h := &handler{Options: o, log: o.Logger}
	return http.HandlerFunc(h.serve), nil
}

func (h *handler) serve(w http.ResponseWriter, r *http.Request) {
	if r.URL.Path == "/healthz" && r.Method == http.MethodGet {
		writeJSON(w, http.StatusOK, map[string]string{"status": "ok"})
		return
	}
	var requestID string
	err := h.route(w, r, &requestID)
	if err != nil {
		h.fail(w, err, requestID)
	}
}

func (h *handler) route(w http.ResponseWriter, r *http.Request, requestID *string) error {
	token := strings.TrimPrefix(r.Header.Get("Authorization"), "Bearer ")
	p, err := h.Auth.Authenticate(r.Context(), token)
	if err != nil {
		return err
	}
	key := r.Header.Get("Idempotency-Key")
	if r.Method == http.MethodPost && key == "" {
		return contracts.NewError("missing_idempotency_key", 400)
	}
	path := r.URL.Path
	ctx := r.Context()
	segments := strings.Split(strings.Trim(path, "/"), "/")
	switch {
	case r.Method == http.MethodGet && path == "/v1/services":
		if !p.HasScope("invoke") {
			return contracts.NewError("scope_denied", 403)
		}
		services, err := h.Store.List(ctx, p)
		if err != nil {
			return err
		}
		type entry struct {
			ID              string `json:"id"`
			Version         int64  `json:"version"`
			Tier            string `json:"tier,omitempty"`
			MaxOutputTokens int64  `json:"max_output_tokens"`
			Price           any    `json:"price"`
		}
		data := make([]entry, 0, len(services))
		for _, s := range services {
			data = append(data, entry{s.ID, s.Version, s.Tier, s.MaxOutputTokens, s.Price})
		}
		writeJSON(w, http.StatusOK, map[string]any{"data": data, "next_cursor": nil})
		return nil
	case r.Method == http.MethodPost && path == "/v1/inferences":
		body, err := readBody(w, r)
		if err != nil {
			return err
		}
		return h.infer(w, r, p, body, key, requestID)
	case len(segments) >= 3 && segments[0] == "v1" && segments[1] == "requests" && len(segments) <= 4:
		id := segments[2]
		switch {
		case len(segments) == 4 && segments[3] == "cancel" && r.Method == http.MethodPost:
			if !p.HasScope("invoke") {
				return contracts.NewError("scope_denied", 403)
			}
			if err := h.Store.Cancel(ctx, p, id); err != nil {
				return err
			}
			writeJSON(w, http.StatusAccepted, map[string]any{"id": id, "cancel_requested": true})
			return nil
		case len(segments) == 3 && r.Method == http.MethodGet:
			if !p.HasScope("invoke") {
				return contracts.NewError("scope_denied", 403)
			}
			rec, err := h.Store.Get(ctx, p, id)
			if err != nil {
				return err
			}
			writeJSON(w, http.StatusOK, map[string]any{"id": rec.ID, "execution_status": rec.Execution, "billing_status": rec.Billing, "service_version": rec.Service.Version})
			return nil
		}
	case r.Method == http.MethodGet && path == "/v1/admin/balance":
		if !p.HasScope("billing:read") {
			return contracts.NewError("scope_denied", 403)
		}
		rows, err := h.Store.Balance(ctx, p)
		if err != nil {
			return err
		}
		writeJSON(w, http.StatusOK, map[string]any{"data": rows})
		return nil
	case r.Method == http.MethodPost && len(segments) == 5 && segments[1] == "admin" && segments[2] == "services" && segments[4] == "credit-rates":
		body, err := readBody(w, r)
		if err != nil {
			return err
		}
		return h.creditRates(w, r, p, segments[3], body)
	case r.Method == http.MethodPost && path == "/v1/admin/service-versions":
		body, err := readBody(w, r)
		if err != nil {
			return err
		}
		svc, err := contracts.ParseService(body)
		if err != nil {
			return err
		}
		published, err := h.Store.Publish(ctx, p, svc, h.Mode, nil)
		if err != nil {
			return err
		}
		writeJSON(w, http.StatusCreated, published)
		return nil
	case r.Method == http.MethodPost && path == "/v1/admin/routing/dry-run":
		if !p.HasScope("routing:write") {
			return contracts.NewError("scope_denied", 403)
		}
		body, err := readBody(w, r)
		if err != nil {
			return err
		}
		in, err := contracts.ParseInvocation(body)
		if err != nil {
			return err
		}
		svc, err := h.Store.GetService(ctx, p, in.Service, in.ServiceVersion)
		if err != nil {
			return err
		}
		candidates, excluded, err := routing.Route(svc, in)
		if err != nil {
			return err
		}
		type candidate struct {
			ID    string  `json:"id"`
			Score float64 `json:"score"`
			Cost  string  `json:"cost"`
		}
		out := make([]candidate, 0, len(candidates))
		for _, c := range candidates {
			out = append(out, candidate{c.Offering.ID, c.Score, c.Cost.String()})
		}
		writeJSON(w, http.StatusOK, map[string]any{"excluded": excluded, "candidates": out})
		return nil
	}
	return contracts.NewError("resource_not_found", 404)
}

func (h *handler) creditRates(w http.ResponseWriter, r *http.Request, p contracts.Principal, id string, body []byte) error {
	var doc map[string]any
	dec := json.NewDecoder(bytes.NewReader(body))
	dec.UseNumber()
	if err := dec.Decode(&doc); err != nil || doc == nil {
		return contracts.NewError("invalid_credit_rates", 400)
	}
	for k := range doc {
		if k != "expected_version" && k != "config" {
			return contracts.NewError("invalid_credit_rates", 400)
		}
	}
	number, ok := doc["expected_version"].(json.Number)
	if !ok {
		return contracts.NewError("invalid_service_version", 400)
	}
	version, err := number.Int64()
	if err != nil {
		return contracts.NewError("invalid_service_version", 400)
	}
	service, err := router.PublishCreditRateVersion(r.Context(), h.Store, p, id, version, doc["config"], h.Mode)
	if err != nil {
		return err
	}
	writeJSON(w, http.StatusCreated, service)
	return nil
}

// infer answers a non-streaming request with JSON, a streaming one with server-sent events. A streaming
// request only turns into an event stream once the request exists: earlier failures are ordinary JSON errors.
func (h *handler) infer(w http.ResponseWriter, r *http.Request, p contracts.Principal, body []byte, key string, requestID *string) error {
	var probe struct {
		Stream bool `json:"stream"`
	}
	_ = json.Unmarshal(body, &probe)
	var emit func(contracts.StreamEvent) error
	streaming := false
	if probe.Stream {
		flusher, _ := w.(http.Flusher)
		emit = func(e contracts.StreamEvent) error {
			if !streaming {
				streaming = true
				*requestID = e.RequestID
				w.Header().Set("Content-Type", "text/event-stream")
				w.Header().Set("Cache-Control", "no-cache")
				w.Header().Set("X-Accel-Buffering", "no")
				w.Header().Set("X-Request-Id", e.RequestID)
				w.WriteHeader(http.StatusOK)
			}
			if err := writeEvent(w, e.Type, eventData(e)); err != nil {
				return err
			}
			if flusher != nil {
				flusher.Flush()
			}
			return nil
		}
	}
	res, err := h.Router.Infer(r.Context(), p, body, key, emit)
	if err != nil {
		if !streaming {
			return err
		}
		// The stream is already open: end it with the failure as a final event.
		code := errorOf(err).Code
		_ = writeEvent(w, "request.failed", map[string]any{"type": "request.failed", "request_id": *requestID, "error": map[string]string{"code": code}})
		if f, ok := w.(http.Flusher); ok {
			f.Flush()
		}
		return nil
	}
	if streaming {
		return nil
	}
	if res.Replayed {
		w.Header().Set("Location", "/v1/requests/"+res.ID)
		w.WriteHeader(http.StatusSeeOther)
		return nil
	}
	w.Header().Set("X-Request-Id", res.ID)
	writeJSON(w, http.StatusOK, res)
	return nil
}

func eventData(e contracts.StreamEvent) map[string]any {
	out := map[string]any{"type": e.Type, "request_id": e.RequestID, "sequence": e.Sequence}
	for k, v := range e.Data {
		out[k] = v
	}
	return out
}

func writeEvent(w io.Writer, kind string, data map[string]any) error {
	raw, err := marshal(data)
	if err != nil {
		return err
	}
	_, err = fmt.Fprintf(w, "event: %s\ndata: %s\n\n", kind, bytes.TrimRight(raw, "\n"))
	return err
}

func readBody(w http.ResponseWriter, r *http.Request) ([]byte, error) {
	if r.ContentLength > MaxBodyBytes {
		return nil, contracts.NewError("body_too_large", 413)
	}
	raw, err := io.ReadAll(http.MaxBytesReader(w, r.Body, MaxBodyBytes))
	if err != nil {
		var tooBig *http.MaxBytesError
		if errors.As(err, &tooBig) {
			return nil, contracts.NewError("body_too_large", 413)
		}
		return nil, contracts.NewError("invalid_json", 400)
	}
	if !json.Valid(raw) {
		return nil, contracts.NewError("invalid_json", 400)
	}
	return raw, nil
}

func errorOf(err error) *contracts.RouterError {
	var re *contracts.RouterError
	if errors.As(err, &re) {
		return re
	}
	return contracts.NewError("internal_error", 500)
}

func (h *handler) fail(w http.ResponseWriter, err error, requestID string) {
	re := errorOf(err)
	retryable := false
	var withRequest *contracts.RouterErrorWithRequest
	if errors.As(err, &withRequest) {
		requestID, retryable = withRequest.RequestID, withRequest.Retryable
	}
	if re.Status >= 500 {
		h.log.Error("request failed", "code", re.Code, "request_id", requestID, "error", err)
	}
	body := map[string]any{"code": re.Code, "retryable": retryable}
	if requestID != "" {
		body["request_id"] = requestID
	}
	writeJSON(w, re.Status, map[string]any{"error": body})
}

func marshal(v any) ([]byte, error) {
	var b bytes.Buffer
	enc := json.NewEncoder(&b)
	enc.SetEscapeHTML(false)
	if err := enc.Encode(v); err != nil {
		return nil, err
	}
	return b.Bytes(), nil
}

func writeJSON(w http.ResponseWriter, status int, v any) {
	raw, err := marshal(v)
	if err != nil {
		http.Error(w, `{"error":{"code":"internal_error","retryable":false}}`, http.StatusInternalServerError)
		return
	}
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	_, _ = w.Write(raw)
}
