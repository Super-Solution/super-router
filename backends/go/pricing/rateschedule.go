// SPDX-License-Identifier: Apache-2.0

package pricing

import (
	"bytes"
	"encoding/json"
	"math/big"
	"sort"
)

// RateStep is one card and the instant it takes effect.
type RateStep struct {
	// FromMs is the Unix milliseconds from which Card is in effect, until the next step's FromMs.
	FromMs int64    `json:"from_ms"`
	Card   RateCard `json:"card"`
}

// RateSchedule holds the cards in the order they take effect.
//
// Providers announce price changes in advance (a rate that doubles on a date). A schedule states the whole plan once,
// and a call is priced by the card in effect when it starts. It is two atoms and no I/O: CardAt is the card in effect
// at an instant, and BoundBetween is what to hold for a call that starts at one instant and may settle at a later
// one, covering a price change in the middle of a call whichever card ends up pricing it.
//
// Times are Unix milliseconds. The schedule does not read a clock and does not parse dates; the caller owns both. A
// schedule with one step is a single card with a start time. Build one with NewRateSchedule or DecodeRateSchedule;
// both validate.
type RateSchedule struct {
	steps []RateStep
}

type scheduleJSON struct {
	Steps []RateStep `json:"steps"`
}

func invalidSchedule() *RouterError { return fail("invalid_rate_schedule", 400) }

// NewRateSchedule fails with invalid_rate_schedule when there is no step, the steps are not in strictly increasing
// FromMs order, or a card is not a valid RateCard.
func NewRateSchedule(steps []RateStep) (RateSchedule, error) {
	if len(steps) == 0 {
		return RateSchedule{}, invalidSchedule()
	}
	for i, s := range steps {
		if i > 0 && steps[i-1].FromMs >= s.FromMs {
			return RateSchedule{}, invalidSchedule()
		}
		if _, err := s.Card.Effective(); err != nil {
			return RateSchedule{}, invalidSchedule()
		}
	}
	return RateSchedule{steps: append([]RateStep(nil), steps...)}, nil
}

// DecodeRateSchedule parses a schedule, refuses keys it does not know and validates it, with invalid_rate_schedule.
func DecodeRateSchedule(data []byte) (RateSchedule, error) {
	var raw scheduleJSON
	d := json.NewDecoder(bytes.NewReader(data))
	d.DisallowUnknownFields()
	if err := d.Decode(&raw); err != nil {
		return RateSchedule{}, invalidSchedule()
	}
	return NewRateSchedule(raw.Steps)
}

// MarshalJSON writes the schedule in the form DecodeRateSchedule reads.
func (s RateSchedule) MarshalJSON() ([]byte, error) {
	return json.Marshal(scheduleJSON{Steps: s.steps})
}

// Steps returns a copy of the steps.
func (s RateSchedule) Steps() []RateStep { return append([]RateStep(nil), s.steps...) }

// CardAt is the card in effect at atMs. It fails with rate_card_not_in_effect before the first step: a price that was
// never announced is refused, not guessed.
func (s RateSchedule) CardAt(atMs int64) (RateCard, error) {
	started := sort.Search(len(s.steps), func(i int) bool { return s.steps[i].FromMs > atMs })
	if started == 0 {
		return RateCard{}, fail("rate_card_not_in_effect", 400)
	}
	return s.steps[started-1].Card, nil
}

// NextChangeAfter is when the card in effect at atMs is replaced, and false if it is the last. For a warning such as
// "the price doubles in two days".
func (s RateSchedule) NextChangeAfter(atMs int64) (int64, bool) {
	for _, step := range s.steps {
		if step.FromMs > atMs {
			return step.FromMs, true
		}
	}
	return 0, false
}

// BoundBetween is what to hold for a call that starts at startMs and settles by endMs: the dearest RateCard.Bound of
// every card in effect at some instant of startMs..endMs. RateCard.Charge with the card in effect at any instant in
// that range, of counts within the limits, is never more. It fails with invalid_usage when endMs is before startMs or
// a limit is negative, and with rate_card_not_in_effect before the first step.
func (s RateSchedule) BoundBetween(startMs, endMs, maxInput, maxOutput int64) (*big.Int, error) {
	if endMs < startMs {
		return nil, invalidCounts()
	}
	first, err := s.CardAt(startMs)
	if err != nil {
		return nil, err
	}
	dearest, err := first.Bound(maxInput, maxOutput)
	if err != nil {
		return nil, err
	}
	for _, step := range s.steps {
		if step.FromMs <= startMs || step.FromMs > endMs {
			continue
		}
		hold, err := step.Card.Bound(maxInput, maxOutput)
		if err != nil {
			return nil, err
		}
		if hold.Cmp(dearest) > 0 {
			dearest = hold
		}
	}
	return dearest, nil
}
