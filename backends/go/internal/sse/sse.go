// SPDX-License-Identifier: Apache-2.0

// Package sse reads server-sent events across arbitrary chunk and CRLF boundaries. Content is never logged.
package sse

import (
	"bufio"
	"io"
	"strings"
)

// MaxFrameBytes caps one event so a hostile upstream cannot exhaust memory.
const MaxFrameBytes = 2_000_000

// Frame is one event: its name ("message" when absent) and its joined data lines.
type Frame struct {
	Event string
	Data  string
}

// ErrFrameTooLarge is returned when an event exceeds MaxFrameBytes.
var ErrFrameTooLarge = &frameTooLarge{}

type frameTooLarge struct{}

func (*frameTooLarge) Error() string { return "sse: frame too large" }

// Read calls yield for each complete event, in order, until the stream ends or yield returns an error. A
// trailing event with no terminating blank line is dropped, as in the TypeScript reference.
func Read(r io.Reader, yield func(Frame) error) error {
	reader := bufio.NewReaderSize(r, 32*1024)
	event, size := "message", 0
	var data []string
	for {
		line, err := readLine(reader, MaxFrameBytes-size)
		if err == ErrFrameTooLarge {
			return err
		}
		size += len(line)
		if line != "" || err == nil {
			trimmed := strings.TrimRight(line, "\r\n")
			switch {
			case err == nil && trimmed == "":
				if len(data) > 0 {
					if yerr := yield(Frame{Event: event, Data: strings.Join(data, "\n")}); yerr != nil {
						return yerr
					}
				}
				event, size, data = "message", 0, nil
			case strings.HasPrefix(trimmed, "event:"):
				event = strings.TrimSpace(trimmed[len("event:"):])
			case strings.HasPrefix(trimmed, "data:"):
				data = append(data, strings.TrimPrefix(trimmed[len("data:"):], " "))
			}
		}
		if err != nil {
			if err == io.EOF {
				return nil
			}
			return err
		}
	}
}

// readLine reads through the next '\n' (or to the end of the stream) but gives up as soon as the line passes
// budget bytes. ReadString would buffer a line that never ends, which is exactly what a hostile upstream sends.
func readLine(r *bufio.Reader, budget int) (string, error) {
	var line []byte
	for {
		chunk, err := r.ReadSlice('\n')
		if len(line)+len(chunk) > budget {
			return "", ErrFrameTooLarge
		}
		line = append(line, chunk...)
		if err == bufio.ErrBufferFull {
			continue
		}
		return string(line), err
	}
}
