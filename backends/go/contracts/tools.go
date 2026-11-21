// SPDX-License-Identifier: Apache-2.0

package contracts

import (
	"bytes"
	"encoding/json"
	"errors"
	"io"
	"regexp"
	"sort"
	"strconv"
	"strings"
)

// ToolCall is one call a model made: arguments are the JSON object the model produced, as a string.
type ToolCall struct {
	ID        string `json:"id"`
	Name      string `json:"name"`
	Arguments string `json:"arguments"`
}

// Tool declares a function the model may call. InputSchema is a JSON Schema whose type is "object".
type Tool struct {
	Name        string     `json:"name"`
	Description *string    `json:"description,omitempty"`
	InputSchema JSONObject `json:"input_schema"`
}

// JSONObject is an arbitrary JSON object, decoded with json.Number so that no number loses its spelling or
// precision on the way to an upstream.
type JSONObject map[string]any

// UnmarshalJSON decodes an object, keeping numbers as json.Number.
func (o *JSONObject) UnmarshalJSON(data []byte) error {
	dec := json.NewDecoder(bytes.NewReader(data))
	dec.UseNumber()
	var m map[string]any
	if err := dec.Decode(&m); err != nil || m == nil {
		return errInvalid
	}
	*o = m
	return nil
}

// ToolChoice says whether and which tool the model must call: Mode is "auto", "none" or "required", or Name
// names one declared tool.
type ToolChoice struct {
	Mode string
	Name string
}

// MarshalJSON renders the wire form: a string, or {"name": ...}.
func (c ToolChoice) MarshalJSON() ([]byte, error) {
	if c.Name != "" {
		return json.Marshal(map[string]string{"name": c.Name})
	}
	return json.Marshal(c.Mode)
}

// UnmarshalJSON accepts exactly the reference's forms.
func (c *ToolChoice) UnmarshalJSON(data []byte) error {
	var mode string
	if json.Unmarshal(data, &mode) == nil {
		if mode != "auto" && mode != "none" && mode != "required" {
			return errInvalid
		}
		*c = ToolChoice{Mode: mode}
		return nil
	}
	var named struct {
		Name string `json:"name"`
	}
	if err := strictDecode(data, &named); err != nil || !toolNameRe.MatchString(named.Name) {
		return errInvalid
	}
	*c = ToolChoice{Name: named.Name}
	return nil
}

// ContentPart is one part of a user message: text, or an image given as an https URL or a base64 data URL.
type ContentPart struct {
	Type   string `json:"type"`
	Text   string `json:"text,omitempty"`
	URL    string `json:"url,omitempty"`
	Detail string `json:"detail,omitempty"`
}

// UnmarshalJSON accepts a text part or an image part, with no other keys.
func (p *ContentPart) UnmarshalJSON(data []byte) error {
	var head struct {
		Type string `json:"type"`
	}
	if json.Unmarshal(data, &head) != nil {
		return errInvalid
	}
	switch head.Type {
	case "text":
		var t struct {
			Type string `json:"type"`
			Text string `json:"text"`
		}
		if strictDecode(data, &t) != nil || !validContent(t.Text) {
			return errInvalid
		}
		*p = ContentPart{Type: "text", Text: t.Text}
	case "image":
		var i struct {
			Type   string  `json:"type"`
			URL    string  `json:"url"`
			Detail *string `json:"detail"`
		}
		if strictDecode(data, &i) != nil || !validImageURL(i.URL) {
			return errInvalid
		}
		*p = ContentPart{Type: "image", URL: i.URL}
		if i.Detail != nil {
			if *i.Detail != "auto" && *i.Detail != "low" && *i.Detail != "high" {
				return errInvalid
			}
			p.Detail = *i.Detail
		}
	default:
		return errInvalid
	}
	return nil
}

// Message is one chat message. A plain message has Role "system", "user" or "assistant" and Content. A user message
// may carry Parts (text and images) instead; an assistant message may carry ToolCalls (Content is then optional);
// a "tool" message answers the call ToolCallID with Content, IsError marking a failed call.
type Message struct {
	Role       string
	Content    string
	Parts      []ContentPart
	ToolCalls  []ToolCall
	ToolCallID string
	IsError    *bool
}

// MarshalJSON renders exactly the keys the message has, so its canonical form matches the reference.
func (m Message) MarshalJSON() ([]byte, error) {
	out := map[string]any{"role": m.Role}
	switch {
	case m.Parts != nil:
		out["content"] = m.Parts
	case m.Content != "" || m.Role == "tool" || m.ToolCalls == nil:
		out["content"] = m.Content
	}
	if m.ToolCalls != nil {
		out["tool_calls"] = m.ToolCalls
	}
	if m.Role == "tool" {
		out["tool_call_id"] = m.ToolCallID
		if m.IsError != nil {
			out["is_error"] = *m.IsError
		}
	}
	return json.Marshal(out)
}

// UnmarshalJSON accepts the four message shapes of the reference and nothing else.
func (m *Message) UnmarshalJSON(data []byte) error {
	var fields map[string]json.RawMessage
	if json.Unmarshal(data, &fields) != nil || fields == nil {
		return errInvalid
	}
	var role string
	if json.Unmarshal(fields["role"], &role) != nil {
		return errInvalid
	}
	only := func(keys ...string) bool {
		for k := range fields {
			found := false
			for _, want := range keys {
				found = found || k == want
			}
			if !found {
				return false
			}
		}
		return true
	}
	text := func(raw json.RawMessage, max int) (string, bool) {
		var s string
		if raw == nil || json.Unmarshal(raw, &s) != nil || length(s) > max {
			return "", false
		}
		return s, true
	}
	out := Message{Role: role}
	switch role {
	case "system", "user", "assistant":
		if _, ok := fields["tool_calls"]; ok {
			if role != "assistant" || !only("role", "content", "tool_calls") {
				return errInvalid
			}
			if raw, ok := fields["content"]; ok {
				s, ok := text(raw, 32000)
				if !ok || s == "" {
					return errInvalid
				}
				out.Content = s
			}
			dec := json.NewDecoder(bytes.NewReader(fields["tool_calls"]))
			dec.DisallowUnknownFields()
			if dec.Decode(&out.ToolCalls) != nil || len(out.ToolCalls) < 1 || len(out.ToolCalls) > 64 {
				return errInvalid
			}
			for _, c := range out.ToolCalls {
				if !toolCallIDRe.MatchString(c.ID) || !toolNameRe.MatchString(c.Name) || length(c.Arguments) > 256000 || !isJSONObject(c.Arguments) {
					return errInvalid
				}
			}
			break
		}
		raw, ok := fields["content"]
		if !ok || !only("role", "content") {
			return errInvalid
		}
		if s, ok := text(raw, 32000); ok {
			if s == "" {
				return errInvalid
			}
			out.Content = s
			break
		}
		if role != "user" || json.Unmarshal(raw, &out.Parts) != nil || out.Parts == nil || len(out.Parts) < 1 || len(out.Parts) > 32 {
			return errInvalid
		}
	case "tool":
		if !only("role", "tool_call_id", "content", "is_error") {
			return errInvalid
		}
		if json.Unmarshal(fields["tool_call_id"], &out.ToolCallID) != nil || !toolCallIDRe.MatchString(out.ToolCallID) {
			return errInvalid
		}
		s, ok := text(fields["content"], 256000)
		if !ok {
			return errInvalid
		}
		out.Content = s
		if raw, ok := fields["is_error"]; ok {
			var b bool
			if json.Unmarshal(raw, &b) != nil {
				return errInvalid
			}
			out.IsError = &b
		}
	default:
		return errInvalid
	}
	*m = out
	return nil
}

// Needs is what a request requires from a provider beyond plain text.
type Needs struct {
	Tools  bool
	Images bool
}

// InvocationNeeds reports whether a request declares tools or carries tool history, and whether it carries images.
func InvocationNeeds(in Invocation) Needs {
	n := Needs{Tools: in.Input.Tools != nil}
	for _, m := range in.Input.Messages {
		if m.Role == "tool" || m.ToolCalls != nil {
			n.Tools = true
		}
		for _, p := range m.Parts {
			if p.Type == "image" {
				n.Images = true
			}
		}
	}
	return n
}

// ProviderCapabilities is what a provider can carry besides text. The zero value means text only, so a request with
// tools or images is refused before any charge rather than silently losing them.
type ProviderCapabilities struct {
	Tools  bool
	Images bool
}

// CapableProvider is a Provider that declares its capabilities. A provider that does not implement it carries text
// only.
type CapableProvider interface {
	Provider
	Capabilities() ProviderCapabilities
}

// CapabilitiesOf returns what a provider declares, nothing for a plain Provider.
func CapabilitiesOf(p Provider) ProviderCapabilities {
	if c, ok := p.(CapableProvider); ok {
		return c.Capabilities()
	}
	return ProviderCapabilities{}
}

// Can reports whether capabilities cover a request's needs.
func (c ProviderCapabilities) Can(n Needs) bool {
	return (!n.Tools || c.Tools) && (!n.Images || c.Images)
}

var (
	toolNameRe   = regexp.MustCompile(`^[a-zA-Z0-9_-]{1,64}$`)
	toolCallIDRe = regexp.MustCompile(`^[a-zA-Z0-9_-]{1,128}$`)
	dataImageRe  = regexp.MustCompile(`^data:image/(png|jpeg|gif|webp);base64,[A-Za-z0-9+/]+={0,2}$`)
	// [^\s] with JavaScript's \s, which also covers the Unicode spaces RE2's \s leaves out.
	httpsImageRe = regexp.MustCompile(`^https://[^\t\n\v\f\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]+$`)
)

func validImageURL(u string) bool {
	if length(u) > 3_000_000 {
		return false
	}
	return dataImageRe.MatchString(u) || utf16Length(u) <= 4096 && httpsImageRe.MatchString(u)
}

// utf16Length is JavaScript's String.length.
func utf16Length(s string) int {
	n := 0
	for _, r := range s {
		n++
		if r > 0xffff {
			n++
		}
	}
	return n
}

// isJSONObject reports whether s parses as a JSON object, as JSON.parse would.
func isJSONObject(s string) bool {
	dec := json.NewDecoder(strings.NewReader(s))
	dec.UseNumber()
	var v any
	if dec.Decode(&v) != nil {
		return false
	}
	if _, err := dec.Token(); err == nil {
		return false
	}
	_, ok := v.(map[string]any)
	return ok && json.Valid([]byte(s))
}

// jsonDepth mirrors the reference: every value at most limit containers deep.
func jsonDepth(v any, limit int) bool {
	if limit < 0 {
		return false
	}
	switch x := v.(type) {
	case []any:
		for _, e := range x {
			if !jsonDepth(e, limit-1) {
				return false
			}
		}
	case map[string]any:
		for _, e := range x {
			if !jsonDepth(e, limit-1) {
				return false
			}
		}
	case JSONObject:
		return jsonDepth(map[string]any(x), limit)
	}
	return true
}

func validTool(t Tool) bool {
	if !toolNameRe.MatchString(t.Name) || t.InputSchema == nil {
		return false
	}
	if d := t.Description; d != nil && (length(*d) < 1 || length(*d) > 4096) {
		return false
	}
	kind, _ := t.InputSchema["type"].(string)
	return kind == "object" && jsonDepth(map[string]any(t.InputSchema), 32)
}

// validToolUse applies the cross-field rules: unique tool names, tool_choice only with tools and naming a declared
// one, no tools on a task, and every tool result answering an earlier, still open call of the step before it.
func validToolUse(in *Invocation) bool {
	tools, choice := in.Input.Tools, in.Input.ToolChoice
	if tools != nil && (len(tools) < 1 || len(tools) > 64) {
		return false
	}
	names := map[string]bool{}
	for _, t := range tools {
		if !validTool(t) || names[t.Name] {
			return false
		}
		names[t.Name] = true
	}
	if in.Input.Task != nil && (tools != nil || choice != nil) {
		return false
	}
	if choice != nil && (tools == nil || choice.Name != "" && !names[choice.Name]) {
		return false
	}
	seen := map[string]bool{}
	open := map[string]bool{}
	for _, m := range in.Input.Messages {
		if m.Role == "tool" {
			if !open[m.ToolCallID] {
				return false
			}
			delete(open, m.ToolCallID)
			continue
		}
		if len(open) > 0 {
			return false
		}
		for _, c := range m.ToolCalls {
			if seen[c.ID] {
				return false
			}
			seen[c.ID] = true
			open[c.ID] = true
		}
	}
	return len(open) == 0
}

// strictDecode decodes one JSON value into a struct with no unknown fields and nothing after it.
func strictDecode(data []byte, into any) error {
	dec := json.NewDecoder(bytes.NewReader(data))
	dec.DisallowUnknownFields()
	if err := dec.Decode(into); err != nil {
		return err
	}
	if dec.More() {
		return errors.New("trailing data")
	}
	return nil
}

// JSTrim is JavaScript's String.prototype.trim: it strips WhiteSpace and LineTerminator code points, a set that
// differs from unicode.IsSpace (U+FEFF is trimmed, U+0085 is not).
func JSTrim(s string) string {
	return strings.TrimFunc(s, isJSSpace)
}

func isJSSpace(r rune) bool {
	switch r {
	case '\t', '\n', '\v', '\f', '\r', ' ', 0x00a0, 0x1680, 0x2028, 0x2029, 0x202f, 0x205f, 0x3000, 0xfeff:
		return true
	}
	return r >= 0x2000 && r <= 0x200a
}

// jsNumber renders a JSON number the way JSON.stringify renders the double it parses to.
func jsNumber(text string) string {
	f, err := strconv.ParseFloat(text, 64)
	if err != nil {
		var ne *strconv.NumError
		if errors.As(err, &ne) && ne.Err == strconv.ErrRange && (f > 1 || f < -1) {
			return "null" // Infinity, which JSON.stringify writes as null
		}
		if errors.As(err, &ne) && ne.Err == strconv.ErrRange {
			f = 0 // underflow to zero
		} else {
			return text
		}
	}
	return FormatJSNumber(f)
}

// FormatJSNumber is Number.prototype.toString for a finite double (and "null" otherwise, like JSON.stringify).
func FormatJSNumber(f float64) string {
	if f != f || f > 1.7976931348623157e308 || f < -1.7976931348623157e308 {
		return "null"
	}
	if f == 0 {
		return "0"
	}
	sign := ""
	if f < 0 {
		sign, f = "-", -f
	}
	// Shortest round-trip digits, as d.ddde±x.
	e := strconv.FormatFloat(f, 'e', -1, 64)
	mant, exp, _ := strings.Cut(e, "e")
	digits := strings.Replace(mant, ".", "", 1)
	x, _ := strconv.Atoi(exp)
	n := x + 1 // value = 0.digits × 10^n
	k := len(digits)
	switch {
	case k <= n && n <= 21:
		return sign + digits + strings.Repeat("0", n-k)
	case 0 < n && n <= 21:
		return sign + digits[:n] + "." + digits[n:]
	case -6 < n && n <= 0:
		return sign + "0." + strings.Repeat("0", -n) + digits
	}
	out := digits[:1]
	if k > 1 {
		out += "." + digits[1:]
	}
	if n-1 >= 0 {
		return sign + out + "e+" + strconv.Itoa(n-1)
	}
	return sign + out + "e-" + strconv.Itoa(1-n)
}

// JSStringify re-encodes JSON text the way JSON.stringify(JSON.parse(text)) does: no whitespace, keys in their
// original order, numbers and string escapes as JavaScript writes them. Providers use it when an upstream hands
// over a tool call's arguments as a parsed object, so every port reports the same arguments string. As in
// JavaScript, the last of duplicate keys wins at the place of the first, and array-index keys come first.
func JSStringify(raw []byte) (string, error) {
	dec := json.NewDecoder(bytes.NewReader(raw))
	dec.UseNumber()
	out, err := jsValue(dec)
	if err != nil {
		return "", err
	}
	if _, err := dec.Token(); err != io.EOF {
		return "", errors.New("contracts: trailing data after JSON value")
	}
	return out, nil
}

// jsValue renders the next value of dec.
func jsValue(dec *json.Decoder) (string, error) {
	tok, err := dec.Token()
	if err != nil {
		return "", err
	}
	var b strings.Builder
	switch x := tok.(type) {
	case json.Delim:
		if x == '[' {
			b.WriteByte('[')
			for i := 0; dec.More(); i++ {
				item, err := jsValue(dec)
				if err != nil {
					return "", err
				}
				if i > 0 {
					b.WriteByte(',')
				}
				b.WriteString(item)
			}
		} else {
			type field struct{ key, value string }
			var fields []field
			at := map[string]int{}
			for dec.More() {
				kt, err := dec.Token()
				if err != nil {
					return "", err
				}
				key, _ := kt.(string)
				value, err := jsValue(dec)
				if err != nil {
					return "", err
				}
				if i, ok := at[key]; ok {
					fields[i].value = value
					continue
				}
				at[key] = len(fields)
				fields = append(fields, field{key, value})
			}
			sort.SliceStable(fields, func(i, j int) bool {
				a, okA := arrayIndex(fields[i].key)
				c, okC := arrayIndex(fields[j].key)
				return okA && (!okC || a < c)
			})
			b.WriteByte('{')
			for i, f := range fields {
				if i > 0 {
					b.WriteByte(',')
				}
				writeString(&b, f.key)
				b.WriteByte(':')
				b.WriteString(f.value)
			}
		}
		if _, err := dec.Token(); err != nil { // the closing delimiter
			return "", err
		}
		b.WriteByte(map[json.Delim]byte{'[': ']', '{': '}'}[x])
	case string:
		writeString(&b, x)
	case json.Number:
		b.WriteString(jsNumber(x.String()))
	case bool:
		b.WriteString(strconv.FormatBool(x))
	case nil:
		b.WriteString("null")
	}
	return b.String(), nil
}

// arrayIndex reports the number a key names when JavaScript treats it as an array index: a canonical integer
// below 2^32 - 1.
func arrayIndex(key string) (uint64, bool) {
	if key == "" || len(key) > 1 && key[0] == '0' {
		return 0, false
	}
	for i := 0; i < len(key); i++ {
		if key[i] < '0' || key[i] > '9' {
			return 0, false
		}
	}
	n, err := strconv.ParseUint(key, 10, 64)
	return n, err == nil && n < 1<<32-1
}
