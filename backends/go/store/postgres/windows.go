// SPDX-License-Identifier: Apache-2.0

package postgres

import (
	"context"
	"database/sql"
	"encoding/json"
	"math/big"
	"time"

	"github.com/Super-Solution/super-router/backends/go/contracts"
	"github.com/Super-Solution/super-router/backends/go/internal/uuid"
)

// weekHours is the fixed length of the long window; only the session window's length is configurable.
const weekHours = 168

// windowSpec is one enforced window of an entitlement's snapshot (docs/WINDOW_LIMITS.md).
type windowSpec struct {
	kind  string
	hours int
	limit *big.Int
}

// windowBlock is a window that held a draw below what was wanted.
type windowBlock struct {
	kind  string
	limit *big.Int
	used  *big.Int
	// resetsAt is when the open window closes; nil when no window is open (the draw alone exceeds the limit).
	resetsAt *time.Time
	// retryable is false when even an empty window could not admit the draw, so waiting does not help.
	retryable bool
}

// worse picks which of two blocking windows to report: one that waiting cannot fix, else the one that resets last.
func (b *windowBlock) worse(than *windowBlock) bool {
	if than == nil {
		return true
	}
	if !b.retryable && than.retryable {
		return true
	}
	if b.retryable != than.retryable {
		return false
	}
	return resetOf(b).After(resetOf(than))
}

func resetOf(b *windowBlock) time.Time {
	if b.resetsAt == nil {
		return time.Time{}
	}
	return *b.resetsAt
}

// windowSpecs reads an entitlement's window_limits snapshot. It is empty when the option is off or absent.
func windowSpecs(raw sql.NullString) ([]windowSpec, error) {
	if !raw.Valid || raw.String == "" || raw.String == "null" {
		return nil, nil
	}
	var w struct {
		Enabled bool `json:"enabled"`
		Session *struct {
			Hours   int    `json:"hours"`
			Credits string `json:"credits"`
		} `json:"session"`
		Weekly *struct {
			Credits string `json:"credits"`
		} `json:"weekly"`
	}
	if err := json.Unmarshal([]byte(raw.String), &w); err != nil {
		return nil, err
	}
	if !w.Enabled {
		return nil, nil
	}
	var out []windowSpec
	if w.Session != nil {
		out = append(out, windowSpec{"session", w.Session.Hours, atoms(w.Session.Credits)})
	}
	if w.Weekly != nil {
		out = append(out, windowSpec{"weekly", weekHours, atoms(w.Weekly.Credits)})
	}
	return out, nil
}

// admit admits up to want of the subscription's credits against every enforced window. It returns how much may be
// drawn (the smallest headroom), the windows that now hold it, and, when a window held the draw below want, the
// one to report. Windows open lazily and only when something is actually drawn. It runs inside the tenant
// transaction, so concurrent reservations of one tenant cannot overshoot a limit.
func (s *Store) admit(ctx context.Context, tx *sql.Tx, p contracts.Principal, subscription string, specs []windowSpec, want *big.Int) (*big.Int, []string, *windowBlock, error) {
	type open struct {
		id      string
		end     time.Time
		spent   *big.Int
		held    *big.Int
		present bool
	}
	rows := make([]open, len(specs))
	take := new(big.Int).Set(want)
	var blocked *windowBlock
	for i, spec := range specs {
		var id, spent, held string
		var end time.Time
		err := tx.QueryRowContext(ctx,
			"SELECT id,window_end,spent::text,held::text FROM sr_usage_windows WHERE workspace=$1 AND environment=$2 AND subscription=$3 AND kind=$4 AND window_end>now() ORDER BY window_start DESC LIMIT 1 FOR UPDATE",
			p.Workspace, p.Environment, subscription, spec.kind).Scan(&id, &end, &spent, &held)
		used := new(big.Int)
		switch {
		case err == nil:
			rows[i] = open{id, end, atoms(spent), atoms(held), true}
			used.Add(rows[i].spent, rows[i].held)
		case err == sql.ErrNoRows:
		default:
			return nil, nil, nil, err
		}
		room := new(big.Int)
		if spec.limit.Cmp(used) > 0 {
			room.Sub(spec.limit, used)
		}
		if room.Cmp(want) < 0 {
			b := &windowBlock{kind: spec.kind, limit: spec.limit, used: used, retryable: want.Cmp(spec.limit) <= 0}
			if rows[i].present {
				end := rows[i].end
				b.resetsAt = &end
			}
			if b.worse(blocked) {
				blocked = b
			}
			if room.Cmp(take) < 0 {
				take = room
			}
		}
	}
	if take.Sign() <= 0 {
		return new(big.Int), nil, blocked, nil
	}
	ids := make([]string, 0, len(specs))
	for i, spec := range specs {
		id := rows[i].id
		if !rows[i].present {
			id = uuid.New()
			if _, err := tx.ExecContext(ctx,
				"INSERT INTO sr_usage_windows(workspace,environment,id,subscription,kind,window_start,window_end) VALUES($1,$2,$3,$4,$5,now(),now()+make_interval(hours=>$6))",
				p.Workspace, p.Environment, id, subscription, spec.kind, spec.hours); err != nil {
				return nil, nil, nil, err
			}
		}
		if _, err := tx.ExecContext(ctx,
			"UPDATE sr_usage_windows SET held=held+$4::numeric WHERE workspace=$1 AND environment=$2 AND id=$3",
			p.Workspace, p.Environment, id, take.String()); err != nil {
			return nil, nil, nil, err
		}
		ids = append(ids, id)
	}
	return take, ids, blocked, nil
}

// settleWindows moves an allocation's hold out of its windows and what was actually charged into them. A window
// left with nothing spent and nothing held (every request in it was released) is dropped, so a request that cost
// nothing does not start anyone's session clock.
func settleWindows(ctx context.Context, tx *sql.Tx, p contracts.Principal, ids []string, held, charged string) error {
	if len(ids) == 0 {
		return nil
	}
	list, _ := json.Marshal(ids)
	if _, err := tx.ExecContext(ctx,
		"UPDATE sr_usage_windows SET held=held-$4::numeric,spent=spent+$5::numeric WHERE workspace=$1 AND environment=$2 AND id=ANY(SELECT jsonb_array_elements_text($3::jsonb))",
		p.Workspace, p.Environment, string(list), held, charged); err != nil {
		return err
	}
	_, err := tx.ExecContext(ctx,
		"DELETE FROM sr_usage_windows WHERE workspace=$1 AND environment=$2 AND id=ANY(SELECT jsonb_array_elements_text($3::jsonb)) AND spent=0 AND held=0",
		p.Workspace, p.Environment, string(list))
	return err
}

// windowError is the customer-facing refusal: which window, how full, when it resets.
func windowError(b *windowBlock) error {
	details := map[string]any{"window": b.kind, "limit": b.limit.String(), "used": b.used.String(), "resets_at": nil}
	if b.resetsAt != nil {
		details["resets_at"] = b.resetsAt.UTC().Format("2006-01-02T15:04:05.000Z")
	}
	e := contracts.NewError("window_limit_reached", 429)
	e.Retryable = b.retryable
	e.Details = details
	return e
}

// WindowRow is one session or weekly window of a paid subscription, as Windows reports it.
type WindowRow struct {
	Subscription string     `json:"subscription"`
	Window       string     `json:"window"`
	Hours        int        `json:"hours"`
	Limit        string     `json:"limit"`
	Spent        string     `json:"spent"`
	Held         string     `json:"held"`
	Remaining    string     `json:"remaining"`
	WindowStart  *time.Time `json:"window_start"`
	ResetsAt     *time.Time `json:"resets_at"`
}

// Windows reports the customer's session/weekly window usage for subscriptions with the option on. The limits are
// those of each subscription's current entitlement. A window that has not opened yet reports nothing spent and no
// reset time: it starts with the next request.
func (s *Store) Windows(ctx context.Context, p contracts.Principal) ([]WindowRow, error) {
	rows, err := s.db.QueryContext(ctx,
		"SELECT DISTINCT ON (subscription) subscription,window_limits::text FROM sr_entitlements WHERE workspace=$1 AND environment=$2 AND customer=$3 AND period_start<=now() AND period_end>now() ORDER BY subscription,cycle DESC",
		p.Workspace, p.Environment, p.Customer)
	if err != nil {
		return nil, err
	}
	type ent struct {
		subscription string
		limits       sql.NullString
	}
	var ents []ent
	for rows.Next() {
		var e ent
		if err := rows.Scan(&e.subscription, &e.limits); err != nil {
			rows.Close()
			return nil, err
		}
		ents = append(ents, e)
	}
	if err := rows.Close(); err != nil {
		return nil, err
	}
	out := []WindowRow{}
	for _, e := range ents {
		specs, err := windowSpecs(e.limits)
		if err != nil {
			return nil, err
		}
		for _, spec := range specs {
			row := WindowRow{Subscription: e.subscription, Window: spec.kind, Hours: spec.hours, Limit: spec.limit.String(), Spent: "0", Held: "0"}
			var start, end time.Time
			var spent, held string
			err := s.db.QueryRowContext(ctx,
				"SELECT window_start,window_end,spent::text,held::text FROM sr_usage_windows WHERE workspace=$1 AND environment=$2 AND subscription=$3 AND kind=$4 AND window_end>now() ORDER BY window_start DESC LIMIT 1",
				p.Workspace, p.Environment, e.subscription, spec.kind).Scan(&start, &end, &spent, &held)
			used := new(big.Int)
			switch {
			case err == nil:
				row.Spent, row.Held, row.WindowStart, row.ResetsAt = spent, held, &start, &end
				used.Add(atoms(spent), atoms(held))
			case err != sql.ErrNoRows:
				return nil, err
			}
			remaining := new(big.Int)
			if spec.limit.Cmp(used) > 0 {
				remaining.Sub(spec.limit, used)
			}
			row.Remaining = remaining.String()
			out = append(out, row)
		}
	}
	return out, nil
}

// WindowsJSON is Windows for hosts that serve it without importing this package (the HTTP server).
func (s *Store) WindowsJSON(ctx context.Context, p contracts.Principal) (any, error) {
	return s.Windows(ctx, p)
}
