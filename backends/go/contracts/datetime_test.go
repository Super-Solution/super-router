// SPDX-License-Identifier: Apache-2.0

package contracts

import "testing"

func TestParseUTCMillis(t *testing.T) {
	for s, want := range map[string]int64{
		"1970-01-01T00:00:00Z":        0,
		"2000-01-01T00:00:00Z":        946_684_800_000,
		"2026-11-01T00:00:00.123456Z": 1_793_491_200_123,
		"2024-02-29T12:00:00Z":        1_709_208_000_000,
	} {
		if got, ok := ParseUTCMillis(s); !ok || got != want {
			t.Errorf("%s: %d %v, want %d", s, got, ok, want)
		}
	}
	for _, bad := range []string{"", "2026-11-01", "2026-11-01T12:30Z", "2026-11-01 12:30:00Z", "2026-02-30T00:00:00Z",
		"2026-11-01T24:00:00Z", "2026-11-01T12:30:00.Z", "2026-11-01T12:30:00+00:00", "2025-02-29T00:00:00Z"} {
		if _, ok := ParseUTCMillis(bad); ok {
			t.Errorf("%q accepted", bad)
		}
	}
}
