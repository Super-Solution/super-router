// SPDX-License-Identifier: Apache-2.0

package openai

import (
	"bytes"
	"encoding/json"
	"math"

	"github.com/Super-Solution/super-router/backends/go/contracts"
)

// Input converts messages to the Responses API `input` list. Plain text messages pass through unchanged; tool calls
// become function_call items and tool results function_call_output items; image parts become input_image.
func Input(messages []contracts.Message) []any {
	out := make([]any, 0, len(messages))
	for _, m := range messages {
		switch {
		case m.Role == "tool":
			out = append(out, map[string]any{"type": "function_call_output", "call_id": m.ToolCallID, "output": m.Content})
		case m.ToolCalls != nil:
			if m.Content != "" {
				out = append(out, map[string]any{"role": "assistant", "content": m.Content})
			}
			for _, c := range m.ToolCalls {
				out = append(out, map[string]any{"type": "function_call", "call_id": c.ID, "name": c.Name, "arguments": c.Arguments})
			}
		case m.Parts != nil:
			parts := make([]any, 0, len(m.Parts))
			for _, p := range m.Parts {
				if p.Type == "text" {
					parts = append(parts, map[string]any{"type": "input_text", "text": p.Text})
					continue
				}
				image := map[string]any{"type": "input_image", "image_url": p.URL}
				if p.Detail != "" {
					image["detail"] = p.Detail
				}
				parts = append(parts, image)
			}
			out = append(out, map[string]any{"role": m.Role, "content": parts})
		default:
			out = append(out, map[string]any{"role": m.Role, "content": m.Content})
		}
	}
	return out
}

// toolFields adds `tools` and `tool_choice` to a request body, and nothing when the request declares no tools.
func toolFields(body map[string]any, in contracts.Input) {
	if in.Tools != nil {
		tools := make([]any, 0, len(in.Tools))
		for _, t := range in.Tools {
			tool := map[string]any{"type": "function", "name": t.Name, "parameters": t.InputSchema, "strict": false}
			if t.Description != nil {
				tool["description"] = *t.Description
			}
			tools = append(tools, tool)
		}
		body["tools"] = tools
	}
	if c := in.ToolChoice; c != nil {
		if c.Name != "" {
			body["tool_choice"] = map[string]any{"type": "function", "name": c.Name}
		} else {
			body["tool_choice"] = c.Mode
		}
	}
}

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

func isNull(raw json.RawMessage) bool { return bytes.Equal(bytes.TrimSpace(raw), []byte("null")) }
