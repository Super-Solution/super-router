// SPDX-License-Identifier: Apache-2.0

package contracts

import (
	"encoding/json"
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

// ParseFailureClass returns the class a stable name stands for, and false for any other text (an error code that is
// not a class, such as "upstream_http_422").
func ParseFailureClass(name string) (FailureClass, bool) {
	for _, c := range []FailureClass{FailureRateLimited, FailureQuotaExhausted, FailureCredentialsRejected, FailureModelUnavailable, FailureRegionBlocked, FailureRequestRejected, FailureUnknown} {
		if c.Name() == name {
			return c, true
		}
	}
	return "", false
}

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

// RetryAfterMaxMs is the longest wait a Retry-After can ask for, in milliseconds. A header that asks for more is read
// as this much, so a hostile or broken value cannot park an offering for days (a health book also has its own ceiling).
const RetryAfterMaxMs int64 = 86_400_000

// ParseRetryAfter reads the value of a Retry-After header as milliseconds. Only the delta-seconds form is read: a
// non-negative number of seconds, whole or with a fraction ("30", "1.5"), surrounding spaces ignored, milliseconds
// rounded down, anything above RetryAfterMaxMs capped. The HTTP-date form is not read (false): it needs a clock the
// provider package does not have, and the APIs this router talks to send seconds.
func ParseRetryAfter(value string) (int64, bool) {
	v := strings.TrimSpace(value)
	whole, fraction, hasDot := strings.Cut(v, ".")
	digits := func(t string) bool {
		for i := 0; i < len(t); i++ {
			if t[i] < '0' || t[i] > '9' {
				return false
			}
		}
		return true
	}
	if whole == "" || !digits(whole) || !digits(fraction) || (hasDot && fraction == "") {
		return 0, false
	}
	// Anything of 10 or more digits is far above the cap, and cannot overflow below.
	var seconds int64 = RetryAfterMaxMs / 1000
	if len(whole) <= 9 {
		seconds, _ = strconv.ParseInt(whole, 10, 64)
	}
	if len(fraction) > 3 {
		fraction = fraction[:3]
	}
	millis, _ := strconv.ParseInt((fraction + "000")[:3], 10, 64)
	return min(seconds*1000+millis, RetryAfterMaxMs), true
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

// RefusalBodyLimit is how much of a client error's body is read for its message.
const RefusalBodyLimit = 16384

// RefusalFrom is the provider error for an HTTP refusal: the status, a client error's body (only the first
// RefusalBodyLimit bytes, and only its error.message is read; nothing of it is passed on) and the Retry-After header.
// Callers do not read a server error's body: the class cannot change what it means. Providers whose error bodies carry
// the message at error.message (OpenAI, Anthropic, Gemini) share this.
func RefusalFrom(status int, retryAfter string, body []byte) *ProviderError {
	var message string
	if status >= 400 && status < 500 {
		if len(body) > RefusalBodyLimit {
			body = body[:RefusalBodyLimit]
		}
		var e struct {
			Error struct {
				Message string `json:"message"`
			} `json:"error"`
		}
		if json.Unmarshal(body, &e) == nil {
			message = e.Error.Message
		}
	}
	err := ClassifyFailure(status, message).ProviderError(status)
	if ms, ok := ParseRetryAfter(retryAfter); ok {
		err.RetryAfterMs = ms
	}
	return err
}
