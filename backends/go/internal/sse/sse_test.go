// SPDX-License-Identifier: Apache-2.0

package sse

import (
	"errors"
	"io"
	"strings"
	"testing"
	"testing/iotest"
)

func collect(t *testing.T, r io.Reader) []Frame {
	t.Helper()
	var out []Frame
	if err := Read(r, func(f Frame) error { out = append(out, f); return nil }); err != nil {
		t.Fatal(err)
	}
	return out
}

func TestFramesAcrossChunkAndCRLFBoundaries(t *testing.T) {
	input := "event: a\r\ndata: one\r\n\r\ndata: two\ndata: lines\n\n: comment\n\nevent: last\ndata:  spaced\n\n"
	want := []Frame{{"a", "one"}, {"message", "two\nlines"}, {"last", " spaced"}}
	for name, r := range map[string]io.Reader{
		"whole":       strings.NewReader(input),
		"one byte":    iotest.OneByteReader(strings.NewReader(input)),
		"half reader": iotest.HalfReader(strings.NewReader(input)),
	} {
		got := collect(t, r)
		if len(got) != len(want) {
			t.Fatalf("%s: %v", name, got)
		}
		for i := range want {
			if got[i] != want[i] {
				t.Errorf("%s frame %d: %+v, want %+v", name, i, got[i], want[i])
			}
		}
	}
}

func TestUnterminatedFrameIsDropped(t *testing.T) {
	if got := collect(t, strings.NewReader("data: x\n\ndata: y")); len(got) != 1 {
		t.Fatalf("%v", got)
	}
}

func TestStopsWhenYieldFails(t *testing.T) {
	boom := errors.New("stop")
	n := 0
	err := Read(strings.NewReader("data: a\n\ndata: b\n\n"), func(Frame) error { n++; return boom })
	if err != boom || n != 1 {
		t.Fatalf("%d %v", n, err)
	}
}

func TestOversizedFrame(t *testing.T) {
	err := Read(strings.NewReader("data: "+strings.Repeat("a", MaxFrameBytes+1)+"\n\n"), func(Frame) error { return nil })
	if !errors.Is(err, ErrFrameTooLarge) {
		t.Fatalf("%v", err)
	}
}

func TestReadError(t *testing.T) {
	boom := errors.New("reset")
	err := Read(io.MultiReader(strings.NewReader("data: a\n\n"), iotest.ErrReader(boom)), func(Frame) error { return nil })
	if !errors.Is(err, boom) {
		t.Fatalf("%v", err)
	}
}

// endless yields bytes with no newline for ever and counts how many were taken.
type endless struct{ taken int }

func (e *endless) Read(p []byte) (int, error) {
	for i := range p {
		p[i] = 'a'
	}
	e.taken += len(p)
	return len(p), nil
}

func TestALineThatNeverEndsIsCutOffNotBuffered(t *testing.T) {
	src := &endless{}
	err := Read(src, func(Frame) error { return nil })
	if !errors.Is(err, ErrFrameTooLarge) {
		t.Fatalf("%v", err)
	}
	if src.taken > MaxFrameBytes+128*1024 {
		t.Fatalf("read %d bytes before giving up", src.taken)
	}
}
