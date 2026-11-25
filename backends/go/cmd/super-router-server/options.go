// SPDX-License-Identifier: Apache-2.0

package main

import (
	"errors"
	"fmt"
	"strconv"
	"strings"

	"github.com/Super-Solution/super-router/backends/go/routing"
)

// maxHealthMs is the longest a configured cooldown can be: one day.
const maxHealthMs = 86_400_000

// baseURL checks a provider base URL: http or https, a host, an optional path prefix and nothing else. The version
// segment is the provider's own (/v1/responses), so a value that already ends in one is refused instead of producing
// /v1/v1/... The value is never echoed: a URL can carry credentials.
func baseURL(name, value string) (string, error) {
	bad := func(why string) error {
		return fmt.Errorf("%s %s (the scheme, host and any path prefix, without the trailing /v1 or /v1beta; value not logged)", name, why)
	}
	rest, ok := strings.CutPrefix(value, "https://")
	if !ok {
		if rest, ok = strings.CutPrefix(value, "http://"); !ok {
			return "", bad("must start with http:// or https://")
		}
	}
	if strings.ContainsFunc(rest, func(r rune) bool { return r < 0x20 || r == 0x7f || r == ' ' || r == '?' || r == '#' }) {
		return "", bad("must have no spaces, query or fragment")
	}
	authority, path, _ := strings.Cut(rest, "/")
	if authority == "" || strings.Contains(authority, "@") {
		return "", bad("must name a host and carry no credentials")
	}
	path = strings.TrimRight(path, "/")
	if last := path[strings.LastIndex(path, "/")+1:]; last == "v1" || last == "v1beta" {
		return "", bad("must not end in the version segment, the provider adds it")
	}
	return strings.TrimRight(value, "/"), nil
}

// healthPolicy reads SUPERROUTER_HEALTH and the policy variables. It returns nil when the health book is off. Policy
// variables without "on" are an error, not a silent no-op.
func healthPolicy(getenv func(string) string) (*routing.HealthPolicy, error) {
	on := false
	switch getenv("SUPERROUTER_HEALTH") {
	case "", "off":
	case "on":
		on = true
	default:
		return nil, errors.New(`SUPERROUTER_HEALTH must be "on" or "off"`)
	}
	policy := routing.DefaultHealthPolicy()
	fields := []struct {
		name  string
		field *int64
	}{
		{"SUPERROUTER_HEALTH_RATE_LIMITED_MS", &policy.RateLimitedMs},
		{"SUPERROUTER_HEALTH_QUOTA_EXHAUSTED_MS", &policy.QuotaExhaustedMs},
		{"SUPERROUTER_HEALTH_CREDENTIALS_REJECTED_MS", &policy.CredentialsRejectedMs},
		{"SUPERROUTER_HEALTH_MODEL_UNAVAILABLE_MS", &policy.ModelUnavailableMs},
		{"SUPERROUTER_HEALTH_REGION_BLOCKED_MS", &policy.RegionBlockedMs},
		{"SUPERROUTER_HEALTH_MAX_MS", &policy.MaxMs},
		{"SUPERROUTER_HEALTH_PROBE_LEASE_MS", &policy.ProbeLeaseMs},
	}
	setAny := false
	for _, f := range fields {
		raw := getenv(f.name)
		if raw == "" {
			continue
		}
		setAny = true
		ms, err := strconv.ParseInt(raw, 10, 64)
		if err != nil || ms < 1 || ms > maxHealthMs {
			return nil, fmt.Errorf("%s must be a whole number of milliseconds from 1 to %d", f.name, maxHealthMs)
		}
		*f.field = ms
	}
	if setAny && !on {
		return nil, errors.New("SUPERROUTER_HEALTH_*_MS needs SUPERROUTER_HEALTH=on")
	}
	if !on {
		return nil, nil
	}
	return &policy, nil
}
