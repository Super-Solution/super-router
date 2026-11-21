// SPDX-License-Identifier: Apache-2.0

package contracts

// ParseUTCMillis reads a strict UTC timestamp, `YYYY-MM-DDTHH:MM:SS[.fraction]Z`, the only form the contracts
// accept, as milliseconds since the Unix epoch (the fraction is cut to milliseconds).
func ParseUTCMillis(s string) (int64, bool) {
	b := []byte(s)
	if len(b) < 20 || b[len(b)-1] != 'Z' || b[4] != '-' || b[7] != '-' || b[10] != 'T' || b[13] != ':' || b[16] != ':' {
		return 0, false
	}
	digits := func(from, to int) (int64, bool) {
		var n int64
		for _, d := range b[from:to] {
			if d < '0' || d > '9' {
				return 0, false
			}
			n = n*10 + int64(d-'0')
		}
		return n, true
	}
	var parts [6]int64
	for i, r := range [][2]int{{0, 4}, {5, 7}, {8, 10}, {11, 13}, {14, 16}, {17, 19}} {
		n, ok := digits(r[0], r[1])
		if !ok {
			return 0, false
		}
		parts[i] = n
	}
	year, month, day, hour, minute, second := parts[0], parts[1], parts[2], parts[3], parts[4], parts[5]
	var millis int64
	if rest := b[19 : len(b)-1]; len(rest) > 0 {
		if rest[0] != '.' || len(rest) < 2 {
			return 0, false
		}
		frac := rest[1:]
		for _, d := range frac {
			if d < '0' || d > '9' {
				return 0, false
			}
		}
		for i := 0; i < 3; i++ {
			millis *= 10
			if i < len(frac) {
				millis += int64(frac[i] - '0')
			}
		}
	}
	if month < 1 || month > 12 || day < 1 || day > daysInMonth(year, month) || hour > 23 || minute > 59 || second > 59 {
		return 0, false
	}
	days := daysFromCivil(year, month, day)
	return ((days*24+hour)*60+minute)*60_000 + second*1000 + millis, true
}

// ValidDatetime reports whether s is a timestamp ParseUTCMillis accepts.
func ValidDatetime(s string) bool { _, ok := ParseUTCMillis(s); return ok }

func daysInMonth(year, month int64) int64 {
	switch month {
	case 2:
		if year%4 == 0 && year%100 != 0 || year%400 == 0 {
			return 29
		}
		return 28
	case 4, 6, 9, 11:
		return 30
	}
	return 31
}

// daysFromCivil counts days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's algorithm).
func daysFromCivil(year, month, day int64) int64 {
	y := year
	if month <= 2 {
		y--
	}
	era := y / 400
	if y < 0 && y%400 != 0 {
		era--
	}
	yoe := y - era*400
	m := month + 9
	if month > 2 {
		m = month - 3
	}
	doy := (153*m+2)/5 + day - 1
	doe := yoe*365 + yoe/4 - yoe/100 + doy
	return era*146_097 + doe - 719_468
}
