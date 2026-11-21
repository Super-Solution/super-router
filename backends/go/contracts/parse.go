// SPDX-License-Identifier: Apache-2.0

package contracts

import (
	"bytes"
	"encoding/json"
	"errors"
	"io"
	"math"
	"regexp"
	"sort"
	"strconv"
	"strings"
	"unicode/utf8"

	"github.com/Super-Solution/super-router/backends/go/pricing"
)

// Media meter names, in the order the reference iterates them.
var MediaKeys = []string{"images", "video_ms", "audio_ms", "tasks"}

// KindMeter maps a task kind to the media meter that bills it.
var KindMeter = map[string]string{"image": "images", "video": "video_ms", "audio": "audio_ms", "task": "tasks"}

const maxSafe = 1<<53 - 1

var (
	idRe    = regexp.MustCompile(`^[a-zA-Z0-9_.:/-]+$`)
	atomsRe = regexp.MustCompile(`^(0|[1-9][0-9]{0,29})$`)
)

var errInvalid = errors.New("invalid")

// InvalidRequest is the error every Parse* function returns for input the reference would reject.
func InvalidRequest() *RouterError { return NewError("invalid_request", 400) }

// length counts code points, as the reference's string limits do.
func length(s string) int { return utf8.RuneCountInString(s) }

func validID(s string) bool { return len(s) >= 1 && length(s) <= 120 && idRe.MatchString(s) }

func safeNonNeg(n int64) bool { return n >= 0 && n <= maxSafe }

// decodeStrict decodes JSON that must contain no null, no duplicate-free surprises and no unknown fields.
// defaults runs on the generic tree first so absent keys get the reference's defaults.
func decodeStrict(data []byte, nulls func(any) bool, defaults func(map[string]any) bool, into any) error {
	dec := json.NewDecoder(bytes.NewReader(data))
	dec.UseNumber()
	var tree any
	if err := dec.Decode(&tree); err != nil {
		return errInvalid
	}
	if _, err := dec.Token(); err != io.EOF {
		return errInvalid
	}
	if nulls(tree) {
		return errInvalid
	}
	root, ok := tree.(map[string]any)
	if !ok {
		return errInvalid
	}
	if !defaults(root) {
		return errInvalid
	}
	normalized, err := json.Marshal(root)
	if err != nil {
		return errInvalid
	}
	dec = json.NewDecoder(bytes.NewReader(normalized))
	dec.DisallowUnknownFields()
	if err := dec.Decode(into); err != nil {
		return errInvalid
	}
	return nil
}

func hasNull(v any) bool {
	switch x := v.(type) {
	case nil:
		return true
	case []any:
		for _, e := range x {
			if hasNull(e) {
				return true
			}
		}
	case map[string]any:
		for _, e := range x {
			if hasNull(e) {
				return true
			}
		}
	}
	return false
}

// invocationHasNull is hasNull except inside a tool's input_schema, where JSON Schema may legitimately hold null.
func invocationHasNull(v any) bool {
	root, ok := v.(map[string]any)
	if !ok {
		return hasNull(v)
	}
	for k, e := range root {
		in, isObject := e.(map[string]any)
		if k != "input" || !isObject {
			if hasNull(e) {
				return true
			}
			continue
		}
		for ik, ie := range in {
			tools, isList := ie.([]any)
			if ik != "tools" || !isList {
				if hasNull(ie) {
					return true
				}
				continue
			}
			for _, t := range tools {
				tool, isTool := t.(map[string]any)
				if !isTool {
					if hasNull(t) {
						return true
					}
					continue
				}
				for tk, te := range tool {
					if tk != "input_schema" && hasNull(te) {
						return true
					}
				}
			}
		}
	}
	return false
}

func setDefault(m map[string]any, key string, value any) {
	if _, ok := m[key]; !ok {
		m[key] = value
	}
}

func object(v any) map[string]any {
	m, _ := v.(map[string]any)
	return m
}

// need reports whether every required key is present. A value that is not an object passes: the typed
// decode rejects it with the right error.
func need(v any, keys ...string) bool {
	m, ok := v.(map[string]any)
	if !ok {
		return true
	}
	for _, k := range keys {
		if _, ok := m[k]; !ok {
			return false
		}
	}
	return true
}

func priceDefaults(v any) bool {
	m := object(v)
	if m == nil {
		return true
	}
	setDefault(m, "per", json.Number("1"))
	if !need(m, "asset", "input", "cached", "cache_write", "output") {
		return false
	}
	media := object(m["media"])
	for _, rate := range media {
		if !need(rate, "atoms", "per") {
			return false
		}
	}
	return true
}

func validPrice(p pricing.Price) bool {
	if !validID(p.Asset) || p.Per < 1 || p.Per > 1_000_000_000 {
		return false
	}
	for _, a := range []string{p.Input, p.Cached, p.CacheWrite, p.Output} {
		if !atomsRe.MatchString(a) {
			return false
		}
	}
	for key, rate := range p.Media {
		if !isMedia(key) || !atomsRe.MatchString(rate.Atoms) || rate.Per < 1 || rate.Per > 1_000_000_000 {
			return false
		}
	}
	return true
}

func isMedia(key string) bool {
	for _, k := range MediaKeys {
		if k == key {
			return true
		}
	}
	return false
}

func validMedia(m map[string]int64) bool {
	for key, n := range m {
		if !isMedia(key) || !safeNonNeg(n) {
			return false
		}
	}
	return true
}

func isTaskKind(k string) bool { _, ok := KindMeter[k]; return ok }

func serviceDefaults(root map[string]any) bool {
	setDefault(root, "required_labels", []any{})
	setDefault(root, "unknown_score", json.Number("0"))
	ok := need(root, "id", "version", "mode", "offerings", "regions", "price", "max_attempts", "max_provider_cost",
		"max_output_tokens", "weights", "cost_scale", "latency_scale_ms")
	ok = ok && need(root["weights"], "cost", "latency", "reliability", "quality")
	ok = ok && priceDefaults(root["price"]) && priceDefaults(root["platform_price"])
	offerings, _ := root["offerings"].([]any)
	for _, o := range offerings {
		if m := object(o); m != nil {
			setDefault(m, "labels", []any{})
			ok = ok && need(m, "id", "provider", "model", "upstream_model", "region", "enabled", "source",
				"max_input_tokens", "max_output_tokens", "price") && priceDefaults(m["price"])
		}
	}
	return ok
}

func validOffering(o *Offering) bool {
	if !validID(o.ID) || !validID(o.Provider) || !validID(o.Model) || !validID(o.UpstreamModel) || !validID(o.Region) {
		return false
	}
	if o.TaskKind != nil && !isTaskKind(*o.TaskKind) {
		return false
	}
	if !validMedia(o.MaxUsage) || o.Labels == nil {
		return false
	}
	for _, l := range o.Labels {
		if !validID(l) {
			return false
		}
	}
	if o.Source != "platform" && o.Source != "byok" {
		return false
	}
	if o.MaxInputTokens < 1 || o.MaxInputTokens > 1_000_000 || o.MaxOutputTokens < 1 || o.MaxOutputTokens > 8192 {
		return false
	}
	if !validPrice(o.Price) {
		return false
	}
	if o.LatencyMs != nil && !(*o.LatencyMs >= 0) {
		return false
	}
	for _, f := range []*float64{o.Reliability, o.Quality} {
		if f != nil && !(*f >= 0 && *f <= 1) {
			return false
		}
	}
	return true
}

// ParseService validates a service definition exactly as the TypeScript reference does: unknown fields,
// out-of-range numbers, malformed ids or amounts, weights that do not sum to one, duplicate offerings and
// mixed supply currencies are all rejected with invalid_request.
func ParseService(data []byte) (Service, error) {
	var s Service
	if err := decodeStrict(data, hasNull, serviceDefaults, &s); err != nil {
		return Service{}, InvalidRequest()
	}
	if !validService(&s) {
		return Service{}, InvalidRequest()
	}
	return s, nil
}

func validService(s *Service) bool {
	if !validID(s.ID) || s.Version < 1 || s.Version > maxSafe {
		return false
	}
	if s.TaskKind != nil && !isTaskKind(*s.TaskKind) {
		return false
	}
	switch s.Mode {
	case "specific":
		if s.Model == "" {
			return false
		}
	case "tier":
		if s.Tier == "" {
			return false
		}
	case "pool":
	default:
		return false
	}
	if s.Model != "" && !validID(s.Model) || s.Tier != "" && !validID(s.Tier) {
		return false
	}
	if len(s.Offerings) < 1 || len(s.Regions) < 1 || s.RequiredLabels == nil {
		return false
	}
	seen := map[string]bool{}
	assets := map[string]bool{}
	for i := range s.Offerings {
		o := &s.Offerings[i]
		if !validOffering(o) || seen[o.ID] {
			return false
		}
		seen[o.ID] = true
		assets[o.Price.Asset] = true
	}
	if len(assets) != 1 {
		return false
	}
	for _, r := range s.Regions {
		if !validID(r) {
			return false
		}
	}
	for _, l := range s.RequiredLabels {
		if !validID(l) {
			return false
		}
	}
	if !validPrice(s.Price) || s.PlatformPrice != nil && !validPrice(*s.PlatformPrice) {
		return false
	}
	if s.MaxAttempts < 1 || s.MaxAttempts > 5 || s.MaxOutputTokens < 1 || s.MaxOutputTokens > 8192 {
		return false
	}
	if !atomsRe.MatchString(s.MaxProviderCost) || !atomsRe.MatchString(s.CostScale) || strings.Trim(s.CostScale, "0") == "" {
		return false
	}
	w := s.Weights
	for _, f := range []float64{w.Cost, w.Latency, w.Reliability, w.Quality} {
		if !(f >= 0) || math.IsInf(f, 0) {
			return false
		}
	}
	if math.Abs(w.Cost+w.Latency+w.Reliability+w.Quality-1) > 1e-9 {
		return false
	}
	return s.LatencyScaleMs > 0 && !math.IsInf(s.LatencyScaleMs, 0) && s.UnknownScore >= 0 && s.UnknownScore <= 1
}

func invocationDefaults(root map[string]any) bool {
	setDefault(root, "payer_mode", "developer")
	setDefault(root, "stream", false)
	ok := need(root, "service", "customer_id", "input", "limits")
	if in := object(root["input"]); in != nil {
		setDefault(in, "messages", []any{})
		ok = ok && need(in["task"], "kind", "prompt")
	}
	if l := object(root["limits"]); l != nil {
		setDefault(l, "max_output_tokens", json.Number("1"))
		setDefault(l, "timeout_ms", json.Number("30000"))
		ok = ok && need(l["max_charge"], "asset", "atoms", "scale")
	}
	return ok
}

// ParseInvocation validates an invocation exactly as the TypeScript reference does and fills its defaults
// (payer_mode "developer", max_output_tokens 1, timeout_ms 30000, stream false, no messages).
func ParseInvocation(data []byte) (Invocation, error) {
	var in Invocation
	if err := decodeStrict(data, invocationHasNull, invocationDefaults, &in); err != nil {
		return Invocation{}, InvalidRequest()
	}
	if !validInvocation(&in) {
		return Invocation{}, InvalidRequest()
	}
	return in, nil
}

func validInvocation(in *Invocation) bool {
	if !validID(in.Service) || !validID(in.CustomerID) || in.PayerMode != "developer" {
		return false
	}
	if in.ServiceVersion != nil && (*in.ServiceVersion < 1 || *in.ServiceVersion > maxSafe) {
		return false
	}
	// Each message's own shape is checked as it is decoded (Message.UnmarshalJSON).
	if in.Input.Messages == nil || len(in.Input.Messages) > 100 || !validToolUse(in) {
		return false
	}
	if t := in.Input.Task; t != nil && (!isTaskKind(t.Kind) || !validContent(t.Prompt)) {
		return false
	}
	l := in.Limits
	if l.MaxOutputTokens < 1 || l.MaxOutputTokens > 8192 || l.TimeoutMs < 100 || l.TimeoutMs > 120000 {
		return false
	}
	if l.MaxUsage != nil && !validMedia(l.MaxUsage) {
		return false
	}
	if c := l.MaxCharge; c != nil && (!validID(c.Asset) || !atomsRe.MatchString(c.Atoms) || c.Scale != 9) {
		return false
	}
	return true
}

func validContent(s string) bool { n := length(s); return n >= 1 && n <= 32000 }

// Valid reports whether every meter is a non-negative safe integer, as the reference requires of provider usage.
func (m Meters) Valid() bool {
	for _, n := range []int64{m.Input, m.Cached, m.CacheWrite, m.Output} {
		if !safeNonNeg(n) {
			return false
		}
	}
	for _, p := range []*int64{m.Images, m.VideoMs, m.AudioMs, m.Tasks} {
		if p != nil && !safeNonNeg(*p) {
			return false
		}
	}
	return true
}

// Canonical renders a value as JSON with object keys sorted by code point (bytewise) and numbers written as
// JSON.stringify writes them, with no whitespace. It is the input of the
// idempotency digest, so two equal invocations always produce the same bytes in every language.
func Canonical(v any) (string, error) {
	raw, err := json.Marshal(v)
	if err != nil {
		return "", err
	}
	dec := json.NewDecoder(bytes.NewReader(raw))
	dec.UseNumber()
	var tree any
	if err := dec.Decode(&tree); err != nil {
		return "", err
	}
	var b strings.Builder
	writeCanonical(&b, tree)
	return b.String(), nil
}

func writeCanonical(b *strings.Builder, v any) {
	switch x := v.(type) {
	case nil:
		b.WriteString("null")
	case bool:
		b.WriteString(strconv.FormatBool(x))
	case json.Number:
		b.WriteString(jsNumber(x.String()))
	case string:
		writeString(b, x)
	case []any:
		b.WriteByte('[')
		for i, e := range x {
			if i > 0 {
				b.WriteByte(',')
			}
			writeCanonical(b, e)
		}
		b.WriteByte(']')
	case map[string]any:
		keys := make([]string, 0, len(x))
		for k := range x {
			keys = append(keys, k)
		}
		sort.Strings(keys)
		b.WriteByte('{')
		for i, k := range keys {
			if i > 0 {
				b.WriteByte(',')
			}
			writeString(b, k)
			b.WriteByte(':')
			writeCanonical(b, x[k])
		}
		b.WriteByte('}')
	}
}

// writeString escapes like JSON.stringify: quotes, backslashes and control characters only.
func writeString(b *strings.Builder, s string) {
	b.WriteByte('"')
	for _, r := range s {
		switch {
		case r == '"':
			b.WriteString(`\"`)
		case r == '\\':
			b.WriteString(`\\`)
		case r == '\b':
			b.WriteString(`\b`)
		case r == '\f':
			b.WriteString(`\f`)
		case r == '\n':
			b.WriteString(`\n`)
		case r == '\r':
			b.WriteString(`\r`)
		case r == '\t':
			b.WriteString(`\t`)
		case r < 0x20:
			b.WriteString(`\u00`)
			b.WriteByte("0123456789abcdef"[r>>4])
			b.WriteByte("0123456789abcdef"[r&15])
		default:
			b.WriteRune(r)
		}
	}
	b.WriteByte('"')
}
