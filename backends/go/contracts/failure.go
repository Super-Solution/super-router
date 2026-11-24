// SPDX-License-Identifier: Apache-2.0

package contracts

import (
	"strconv"
	"strings"
)

// FailureClass says what an upstream refusal means for the next offering.
//
// A failed call says two different things. Some failures belong to the request (a malformed body, an argument the
// model refuses): every other offering would refuse it too, so the call stops. Others belong to the offering (its
// key was rejected, its model is gone, its account is out of credit, its region is blocked, it is rate limited): the
// request is fine and another offering may serve it. ClassifyFailure reads the status and the error's own message, so
// a provider package writes no table of its own. Server errors are deliberately not certain: the provider may have
// started work, so the reservation stays held.
type FailureClass string

// The failure classes.
const (
	FailureRateLimited         FailureClass = "rate_limited"
	FailureQuotaExhausted      FailureClass = "quota_exhausted"
	FailureCredentialsRejected FailureClass = "credentials_rejected"
	FailureModelUnavailable    FailureClass = "model_unavailable"
	FailureRegionBlocked       FailureClass = "region_blocked"
	FailureRequestRejected     FailureClass = "request_rejected"
	FailureUnknown             FailureClass = "unknown"
)

// Name is the stable lower-case name, usable as an error code or a metric label.
func (c FailureClass) Name() string { return string(c) }

// IsOfferingScoped reports whether the failure belongs to the offering, so that another offering may be tried and
// this one should be avoided for a while.
func (c FailureClass) IsOfferingScoped() bool {
	switch c {
	case FailureRateLimited, FailureQuotaExhausted, FailureCredentialsRejected, FailureModelUnavailable, FailureRegionBlocked:
		return true
	}
	return false
}

// ProviderError is the provider error for this class. status is the HTTP status, kept in the code of the two classes
// that do not name their cause. An offering-scoped failure is definitive and retryable; a rejected request is
// definitive and not retryable; anything else keeps the hold.
func (c FailureClass) ProviderError(status int) *ProviderError {
	switch {
	case c.IsOfferingScoped():
		return &ProviderError{Definitive: true, Retryable: true, Code: c.Name()}
	case c == FailureRequestRejected:
		return &ProviderError{Definitive: true, Retryable: false, Code: "upstream_http_" + strconv.Itoa(status)}
	default:
		return &ProviderError{Code: "upstream_http_" + strconv.Itoa(status)}
	}
}

// Phrases are written with spaces: the message is lower-cased and its underscores and hyphens read as spaces first.
var (
	credentialPhrases = []string{"api key not valid", "api key expired", "api key invalid", "invalid api key", "incorrect api key", "invalid x api key"}
	quotaPhrases      = []string{
		"insufficient quota", "exceeded your current quota", "credit balance is too low", "credits are depleted",
		"spend limit", "spending cap", "requires billing to be enabled", "billing account", "per day",
	}
	regionPhrases = []string{"user location is not supported", "not available in your country", "not available in your region", "not supported in your region"}
	modelPhrases  = []string{"model not found", "is not found for api version", "does not exist or you do not have access"}
)

func said(message string, phrases []string) bool {
	for _, p := range phrases {
		if strings.Contains(message, p) {
			return true
		}
	}
	return false
}

// ClassifyFailure classifies a refusal from its HTTP status and the error's own message (may be empty).
func ClassifyFailure(status int, message string) FailureClass {
	m := strings.NewReplacer("_", " ", "-", " ").Replace(strings.ToLower(message))
	if said(m, credentialPhrases) {
		return FailureCredentialsRejected
	}
	if said(m, regionPhrases) {
		return FailureRegionBlocked
	}
	switch {
	case status == 401:
		return FailureCredentialsRejected
	case status == 402:
		return FailureQuotaExhausted
	case (status == 403 || status == 429 || status == 400) && said(m, quotaPhrases):
		return FailureQuotaExhausted
	case status == 403:
		return FailureCredentialsRejected
	case status == 404:
		return FailureModelUnavailable
	case status == 429:
		return FailureRateLimited
	case status == 400 && said(m, modelPhrases):
		return FailureModelUnavailable
	case status >= 400 && status < 500 && status != 408:
		return FailureRequestRejected
	}
	return FailureUnknown
}
