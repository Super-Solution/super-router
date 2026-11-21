// SPDX-License-Identifier: Apache-2.0

package contracts

import (
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"reflect"
	"testing"
)

type parseCase struct {
	Name   string          `json:"name"`
	Input  json.RawMessage `json:"input"`
	Expect struct {
		Ok        json.RawMessage `json:"ok"`
		Canonical string          `json:"canonical"`
		Error     string          `json:"error"`
	} `json:"expect"`
}

func generic(t *testing.T, v any) any {
	t.Helper()
	raw, err := json.Marshal(v)
	if err != nil {
		t.Fatal(err)
	}
	var out any
	if err := json.Unmarshal(raw, &out); err != nil {
		t.Fatal(err)
	}
	return out
}

func TestParseConformance(t *testing.T) {
	raw, err := os.ReadFile(filepath.Join("..", "..", "..", "conformance", "contracts.v1.json"))
	if err != nil {
		t.Fatal(err)
	}
	var data struct {
		Service    []parseCase `json:"service"`
		Invocation []parseCase `json:"invocation"`
	}
	if err := json.Unmarshal(raw, &data); err != nil {
		t.Fatal(err)
	}
	if len(data.Service) < 50 || len(data.Invocation) < 30 {
		t.Fatalf("only %d service and %d invocation cases", len(data.Service), len(data.Invocation))
	}
	check := func(c parseCase, got any, err error, canon func() (string, error)) {
		t.Helper()
		if c.Expect.Error != "" {
			var re *RouterError
			if !errors.As(err, &re) || re.Code != c.Expect.Error || re.Status != 400 {
				t.Errorf("%s: error %v, want %s", c.Name, err, c.Expect.Error)
			}
			return
		}
		if err != nil {
			t.Errorf("%s: %v", c.Name, err)
			return
		}
		var want any
		if err := json.Unmarshal(c.Expect.Ok, &want); err != nil {
			t.Fatal(err)
		}
		if have := generic(t, got); !reflect.DeepEqual(have, want) {
			t.Errorf("%s:\n got  %v\n want %v", c.Name, have, want)
		}
		if c.Expect.Canonical != "" {
			if have, err := canon(); err != nil || have != c.Expect.Canonical {
				t.Errorf("%s: canonical\n got  %s\n want %s (%v)", c.Name, have, c.Expect.Canonical, err)
			}
		}
	}
	for _, c := range data.Service {
		s, err := ParseService(c.Input)
		check(c, s, err, nil)
	}
	for _, c := range data.Invocation {
		in, err := ParseInvocation(c.Input)
		check(c, in, err, func() (string, error) { return Canonical(in) })
	}
}

func TestParseRejectsGarbage(t *testing.T) {
	for _, raw := range []string{``, `null`, `[]`, `"x"`, `{`, `{} {}`, `{"a":1}`} {
		if _, err := ParseInvocation([]byte(raw)); err == nil {
			t.Errorf("invocation %q accepted", raw)
		}
		if _, err := ParseService([]byte(raw)); err == nil {
			t.Errorf("service %q accepted", raw)
		}
	}
}

func FuzzParse(f *testing.F) {
	f.Add([]byte(`{"service":"s","customer_id":"c","input":{},"limits":{}}`))
	f.Add([]byte(`{"id":"s","version":1}`))
	f.Add([]byte(`{"service":"s","customer_id":"c","input":{"messages":[{"role":"user","content":[{"type":"image","url":"https://e.example/a"}]},{"role":"assistant","tool_calls":[{"id":"a","name":"a","arguments":"{}"}]},{"role":"tool","tool_call_id":"a","content":""}],"tools":[{"name":"a","input_schema":{"type":"object","x":null}}],"tool_choice":{"name":"a"}},"limits":{}}`))
	f.Fuzz(func(t *testing.T, data []byte) {
		// Parsing untrusted input must never panic, and an accepted invocation must have a canonical form.
		if in, err := ParseInvocation(data); err == nil {
			if _, err := Canonical(in); err != nil {
				t.Fatal(err)
			}
		}
		_, _ = ParseService(data)
	})
}
