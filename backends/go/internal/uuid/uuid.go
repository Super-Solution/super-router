// SPDX-License-Identifier: Apache-2.0

// Package uuid generates random (version 4) UUIDs without a dependency.
package uuid

import (
	"crypto/rand"
	"fmt"
)

// New returns a random UUID such as "3f1c2a8e-9d1b-4c53-8a52-0c1d5b7e9f10".
func New() string {
	var b [16]byte
	if _, err := rand.Read(b[:]); err != nil {
		panic("uuid: no randomness: " + err.Error())
	}
	b[6] = b[6]&0x0f | 0x40
	b[8] = b[8]&0x3f | 0x80
	return fmt.Sprintf("%x-%x-%x-%x-%x", b[0:4], b[4:6], b[6:8], b[8:10], b[10:])
}
