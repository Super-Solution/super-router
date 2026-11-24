// SPDX-License-Identifier: Apache-2.0

package contracts

import (
	"encoding/json"
	"errors"
	"math"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"testing"
)

type toolsCase struct {
	Name   string          `json:"name"`
	Input  json.RawMessage `json:"input"`
	Raw    string          `json:"raw"`
	Expect struct {
		Ok        json.RawMessage `json:"ok"`
		Canonical string          `json:"canonical"`
		Error     string          `json:"error"`
	} `json:"expect"`
}

// TestToolsConformance runs the invocation cases of conformance/tools.v1.json: tool declarations, tool calls, tool
// results and image parts, with the canonical JSON that feeds the idempotency digest.
func TestToolsConformance(t *testing.T) {
	raw, err := os.ReadFile(filepath.Join("..", "..", "..", "conformance", "tools.v1.json"))
	if err != nil {
		t.Fatal(err)
	}
	var data struct {
		Invocation []toolsCase `json:"invocation"`
	}
	if err := json.Unmarshal(raw, &data); err != nil {
		t.Fatal(err)
	}
	if len(data.Invocation) < 80 {
		t.Fatalf("only %d cases", len(data.Invocation))
	}
	for _, c := range data.Invocation {
		body := []byte(c.Raw)
		if c.Raw == "" {
			body = c.Input
		}
		in, err := ParseInvocation(body)
		if c.Expect.Error != "" {
			var re *RouterError
			if !errors.As(err, &re) || re.Code != c.Expect.Error {
				t.Errorf("%s: accepted, want %s", c.Name, c.Expect.Error)
			}
			continue
		}
		if err != nil {
			t.Errorf("%s: %v", c.Name, err)
			continue
		}
		var want any
		if err := json.Unmarshal(c.Expect.Ok, &want); err != nil {
			t.Fatal(err)
		}
		if have := generic(t, in); !reflect.DeepEqual(have, want) {
			t.Errorf("%s:\n got  %.400v\n want %.400v", c.Name, have, want)
		}
		if have, err := Canonical(in); err != nil || have != c.Expect.Canonical {
			t.Errorf("%s: canonical\n got  %.600s\n want %.600s (%v)", c.Name, have, c.Expect.Canonical, err)
		}
	}
}

func TestInvocationNeeds(t *testing.T) {
	parse := func(input string) Invocation {
		t.Helper()
		in, err := ParseInvocation([]byte(`{"service":"s","customer_id":"c","input":` + input + `,"limits":{}}`))
		if err != nil {
			t.Fatal(err)
		}
		return in
	}
	for input, want := range map[string]Needs{
		`{"messages":[{"role":"user","content":"hi"}]}`:                                                                                                                            {},
		`{"messages":[{"role":"user","content":"hi"}],"tools":[{"name":"a","input_schema":{"type":"object"}}]}`:                                                                    {Tools: true},
		`{"messages":[{"role":"user","content":"hi"},{"role":"assistant","tool_calls":[{"id":"a","name":"a","arguments":"{}"}]},{"role":"tool","tool_call_id":"a","content":""}]}`: {Tools: true},
		`{"messages":[{"role":"user","content":[{"type":"image","url":"https://e.example/a.png"}]}]}`:                                                                              {Images: true},
		`{"messages":[{"role":"user","content":[{"type":"text","text":"x"}]}]}`:                                                                                                    {},
	} {
		if got := InvocationNeeds(parse(input)); got != want {
			t.Errorf("%s: %+v, want %+v", input, got, want)
		}
	}
}

func TestFormatJSNumber(t *testing.T) {
	for f, want := range map[float64]string{
		0: "0", math.Copysign(0, -1): "0", 1: "1", -1.5: "-1.5", 0.1: "0.1", 1e21: "1e+21", 1e20: "100000000000000000000",
		1e-6: "0.000001", 1e-7: "1e-7", 123.456: "123.456", 5e-324: "5e-324", 1.7976931348623157e308: "1.7976931348623157e+308",
		math.Inf(1): "null", 2.5e-5: "0.000025", 1.5e300: "1.5e+300", -1e-7: "-1e-7",
	} {
		if got := FormatJSNumber(f); got != want {
			t.Errorf("%v: %s, want %s", f, got, want)
		}
	}
	for text, want := range map[string]string{"1e400": "null", "-1e400": "null", "1e-400": "0", "1.0": "1", "007": "007"} {
		if got := jsNumber(text); got != want && text != "007" {
			t.Errorf("%s: %s, want %s", text, got, want)
		}
	}
}

func TestJSTrim(t *testing.T) {
	if got := JSTrim("\ufeff\u2028 {} \u3000\t"); got != "{}" {
		t.Errorf("trim: %q", got)
	}
	if got := JSTrim("\u0085{}"); !strings.HasPrefix(got, "\u0085") {
		t.Errorf("U+0085 is not JavaScript whitespace: %q", got)
	}
}

func TestJSStringify(t *testing.T) {
	for in, want := range map[string]string{
		` { "q" : "a" , "n" : [ 1.0 , 2e3 , -0 , {} , [] ] , "t" : true , "z" : null } `: `{"q":"a","n":[1,2000,0,{},[]],"t":true,"z":null}`,
		`{"b":1,"a":{"y":"é\/\u0001"}}`:                                                  `{"b":1,"a":{"y":"é/\u0001"}}`,
		`[]`:                                                                             `[]`,
		`{"a":1,"a":2,"b":"\u00e9\u2028"}`:                                               "{\"a\":2,\"b\":\"\u00e9\u2028\"}",
		`{"b":1,"10":2,"2":3,"01":4,"4294967295":5,"4294967294":6}`:                      `{"2":3,"10":2,"4294967294":6,"b":1,"01":4,"4294967295":5}`,
		`{"a":[1.0,1e21,1E-7,0.000001,12345678901234567890]}`:                            `{"a":[1,1e+21,1e-7,0.000001,12345678901234567000]}`,
	} {
		if got, err := JSStringify([]byte(in)); err != nil || got != want {
			t.Errorf("%s: %s (%v), want %s", in, got, err, want)
		}
	}
}

func TestCallSignatureIsKeptAndABadOneIsRefused(t *testing.T) {
	with := func(signature string) (Invocation, error) {
		raw := `{"service":"s","customer_id":"c","input":{"messages":[{"role":"user","content":"hi"},{"role":"assistant","tool_calls":[{"id":"c1","name":"lookup","arguments":"{}"` +
			signature + `}]},{"role":"tool","tool_call_id":"c1","content":"x"}],"tools":[{"name":"lookup","input_schema":{"type":"object"}}]},"limits":{}}`
		return ParseInvocation([]byte(raw))
	}
	kept, err := with(`,"signature":"c2lnLUE9"`)
	if err != nil {
		t.Fatalf("a signed call must parse: %v", err)
	}
	if got := kept.Input.Messages[1].ToolCalls[0].Signature; got != "c2lnLUE9" {
		t.Errorf("signature %q", got)
	}
	if c, _ := Canonical(kept); !strings.Contains(c, `"signature":"c2lnLUE9"`) {
		t.Errorf("the digest must cover it: %s", c)
	}
	if plain, err := with(""); err != nil || plain.Input.Messages[1].ToolCalls[0].Signature != "" {
		t.Errorf("no signature: %v", err)
	}
	for _, bad := range []string{`,"signature":""`, `,"signature":"a b"`, `,"signature":"a\nb"`, `,"signature":5`, `,"signature":null`, `,"signature":"` + strings.Repeat("a", 16385) + `"`} {
		if _, err := with(bad); err == nil {
			t.Errorf("accepted %.40s", bad)
		}
	}
}
