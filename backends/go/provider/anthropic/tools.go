// SPDX-License-Identifier: Apache-2.0

package anthropic

import (
	"bytes"
	"encoding/json"
	"math"
	"strings"

	"github.com/Super-Solution/super-router/backends/go/contracts"
)

// Messages converts messages to the Messages API `messages` list. System messages are left out (they travel as
// `system`), plain text messages pass through unchanged, tool calls become tool_use blocks and the results of one
// step travel together as tool_result blocks in a single user message.
func Messages(messages []contracts.Message) []any {
	out := []any{}
	var results *[]any
	for _, m := range messages {
		if m.Role == "system" {
			continue
		}
		if m.Role == "tool" {
			block := map[string]any{"type": "tool_result", "tool_use_id": m.ToolCallID, "content": m.Content}
			if m.IsError != nil && *m.IsError {
				block["is_error"] = true
			}
			if results != nil {
				*results = append(*results, block)
				continue
			}
			list := []any{block}
			results = &list
			out = append(out, resultMessage{results})
			continue
		}
		results = nil
		switch {
		case m.ToolCalls != nil:
			content := []any{}
			if m.Content != "" {
				content = append(content, map[string]any{"type": "text", "text": m.Content})
			}
			for _, c := range m.ToolCalls {
				content = append(content, map[string]any{"type": "tool_use", "id": c.ID, "name": c.Name, "input": json.RawMessage(c.Arguments)})
			}
			out = append(out, map[string]any{"role": "assistant", "content": content})
		case m.Parts != nil:
			parts := make([]any, 0, len(m.Parts))
			for _, p := range m.Parts {
				if p.Type == "text" {
					parts = append(parts, map[string]any{"type": "text", "text": p.Text})
					continue
				}
				source := map[string]any{"type": "url", "url": p.URL}
				if rest, ok := strings.CutPrefix(p.URL, "data:"); ok {
					media, data, _ := strings.Cut(rest, ";base64,")
					source = map[string]any{"type": "base64", "media_type": media, "data": data}
				}
				parts = append(parts, map[string]any{"type": "image", "source": source})
			}
			out = append(out, map[string]any{"role": m.Role, "content": parts})
		default:
			out = append(out, map[string]any{"role": m.Role, "content": m.Content})
		}
	}
	return out
}

// resultMessage is the user message that gathers one step's tool results; it is filled as the results arrive.
type resultMessage struct{ content *[]any }

func (r resultMessage) MarshalJSON() ([]byte, error) {
	return json.Marshal(map[string]any{"role": "user", "content": *r.content})
}

// toolFields adds `tools` and `tool_choice` to a request body, and nothing when the request declares no tools.
func toolFields(body map[string]any, in contracts.Input) {
	if in.Tools != nil {
		tools := make([]any, 0, len(in.Tools))
		for _, t := range in.Tools {
			tool := map[string]any{"name": t.Name, "input_schema": t.InputSchema}
			if t.Description != nil {
				tool["description"] = *t.Description
			}
			tools = append(tools, tool)
		}
		body["tools"] = tools
	}
	if c := in.ToolChoice; c != nil {
		switch {
		case c.Name != "":
			body["tool_choice"] = map[string]any{"type": "tool", "name": c.Name}
		case c.Mode == "required":
			body["tool_choice"] = map[string]any{"type": "any"}
		default:
			body["tool_choice"] = map[string]any{"type": c.Mode}
		}
	}
}

func isNull(raw json.RawMessage) bool { return bytes.Equal(bytes.TrimSpace(raw), []byte("null")) }

// safeIndex reads a JSON number that is a safe integer, as Number.isSafeInteger accepts.
func safeIndex(raw json.RawMessage) (int, bool) {
	var f float64
	if raw == nil || isNull(raw) || json.Unmarshal(raw, &f) != nil || f != math.Trunc(f) || math.Abs(f) > 1<<53-1 {
		return 0, false
	}
	return int(f), true
}

// text reads a JSON string; null is not one.
func text(raw json.RawMessage) (string, bool) {
	var s string
	if raw == nil || isNull(raw) || json.Unmarshal(raw, &s) != nil {
		return "", false
	}
	return s, true
}

// wholeInput returns a tool_use block's input when it is a non-empty object, re-encoded as JSON.stringify would.
func wholeInput(raw json.RawMessage) (string, bool) {
	var fields map[string]json.RawMessage
	if raw == nil || isNull(raw) || json.Unmarshal(raw, &fields) != nil || len(fields) == 0 {
		return "", false
	}
	s, err := contracts.JSStringify(raw)
	return s, err == nil
}
