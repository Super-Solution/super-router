// SPDX-License-Identifier: Apache-2.0

package gemini

import (
	"bytes"
	"encoding/json"
	"errors"
	"io"
	"strings"

	"github.com/Super-Solution/super-router/backends/go/contracts"
)

// errRefused marks a request the upstream would reject: a tool call whose arguments are not a JSON object, a tool
// result for a call that was never made, or an image that is a link (Gemini fetches only its own file URIs). Nothing
// is dispatched.
var errRefused = errors.New("request refused")

// contents converts messages to the `contents` list. System messages are left out (they travel as
// `systemInstruction`), tool calls become functionCall parts and the results of one step travel together as
// functionResponse parts in a single user turn. The assistant role is `model`, and a tool result names the function
// it answers, which it finds from the call id.
func contents(messages []contracts.Message) ([]any, error) {
	out := []any{}
	names := map[string]string{}
	var results *[]any
	for _, m := range messages {
		if m.Role == "system" {
			continue
		}
		if m.Role == "tool" {
			name, ok := names[m.ToolCallID]
			if !ok {
				return nil, errRefused
			}
			key := "output"
			if m.IsError != nil && *m.IsError {
				key = "error"
			}
			part := map[string]any{"functionResponse": map[string]any{"name": name, "response": map[string]any{key: m.Content}}}
			if results != nil {
				*results = append(*results, part)
				continue
			}
			list := []any{part}
			results = &list
			out = append(out, resultTurn{results})
			continue
		}
		results = nil
		role := "user"
		if m.Role == "assistant" {
			role = "model"
		}
		switch {
		case m.ToolCalls != nil:
			parts := []any{}
			if m.Content != "" {
				parts = append(parts, map[string]any{"text": m.Content})
			}
			for _, c := range m.ToolCalls {
				names[c.ID] = c.Name
				args, err := arguments(c.Arguments)
				if err != nil {
					return nil, err
				}
				parts = append(parts, map[string]any{"functionCall": map[string]any{"name": c.Name, "args": args}})
			}
			out = append(out, map[string]any{"role": role, "parts": parts})
		case m.Parts != nil:
			parts := make([]any, 0, len(m.Parts))
			for _, p := range m.Parts {
				if p.Type == "text" {
					parts = append(parts, map[string]any{"text": p.Text})
					continue
				}
				rest, ok := strings.CutPrefix(p.URL, "data:")
				media, data, found := strings.Cut(rest, ";base64,")
				if !ok || !found {
					return nil, errRefused
				}
				parts = append(parts, map[string]any{"inlineData": map[string]any{"mimeType": media, "data": data}})
			}
			out = append(out, map[string]any{"role": role, "parts": parts})
		default:
			out = append(out, map[string]any{"role": role, "parts": []any{map[string]any{"text": m.Content}}})
		}
	}
	return out, nil
}

// resultTurn is the user turn that gathers one step's tool results; it is filled as the results arrive.
type resultTurn struct{ parts *[]any }

func (r resultTurn) MarshalJSON() ([]byte, error) {
	return json.Marshal(map[string]any{"role": "user", "parts": *r.parts})
}

// arguments reads a call's arguments as the object Gemini wants. No arguments at all is an empty object.
func arguments(text string) (map[string]any, error) {
	if strings.TrimSpace(text) == "" {
		return map[string]any{}, nil
	}
	dec := json.NewDecoder(strings.NewReader(text))
	dec.UseNumber()
	var v map[string]any
	if dec.Decode(&v) != nil || v == nil {
		return nil, errRefused
	}
	if _, err := dec.Token(); err != io.EOF {
		return nil, errRefused
	}
	return v, nil
}

// toolFields adds `tools` and `toolConfig` to a request body, and nothing when the request declares no tools. The
// schema travels as `parametersJsonSchema`, which takes JSON Schema as written; the older `parameters` field accepts
// only an OpenAPI subset.
func toolFields(body map[string]any, in contracts.Input) {
	if in.Tools != nil {
		declarations := make([]any, 0, len(in.Tools))
		for _, t := range in.Tools {
			d := map[string]any{"name": t.Name, "parametersJsonSchema": t.InputSchema}
			if t.Description != nil {
				d["description"] = *t.Description
			}
			declarations = append(declarations, d)
		}
		body["tools"] = []any{map[string]any{"functionDeclarations": declarations}}
	}
	c := in.ToolChoice
	if c == nil {
		return
	}
	var config map[string]any
	switch {
	case c.Name != "":
		config = map[string]any{"mode": "ANY", "allowedFunctionNames": []any{c.Name}}
	case c.Mode == "none":
		config = map[string]any{"mode": "NONE"}
	case c.Mode == "required":
		config = map[string]any{"mode": "ANY"}
	default:
		config = map[string]any{"mode": "AUTO"}
	}
	body["toolConfig"] = map[string]any{"functionCallingConfig": config}
}

// compactArgs renders a tool call's arguments as compact JSON with sorted keys and no HTML escaping, the way the
// Rust provider does.
func compactArgs(v map[string]any) (string, error) {
	var buf bytes.Buffer
	enc := json.NewEncoder(&buf)
	enc.SetEscapeHTML(false)
	if err := enc.Encode(v); err != nil {
		return "", err
	}
	return strings.TrimSuffix(buf.String(), "\n"), nil
}
