// SPDX-License-Identifier: Apache-2.0

package memory_test

import (
	"sort"
	"sync/atomic"
	"testing"
	"time"

	"github.com/Super-Solution/super-router/backends/go/contracts"
	"github.com/Super-Solution/super-router/backends/go/internal/dataplanetest"
	"github.com/Super-Solution/super-router/backends/go/store/memory"
)

func harness(*testing.T) *dataplanetest.Harness {
	var offset atomic.Int64
	clock := func() time.Time { return time.Now().Add(time.Duration(offset.Load())) }
	store := memory.New(clock)
	return &dataplanetest.Harness{
		Store:   store,
		Clock:   clock,
		Advance: func(ms int64) { offset.Add(ms * int64(time.Millisecond)) },
		Buckets: func(p contracts.Principal) []dataplanetest.BucketRow {
			rows := []dataplanetest.BucketRow{}
			for _, b := range store.Inspect(p).Buckets {
				rows = append(rows, dataplanetest.BucketRow{Source: b.Source, Remaining: b.Remaining, Held: b.Held})
			}
			sort.Slice(rows, func(i, j int) bool { return rows[i].Source < rows[j].Source })
			return rows
		},
	}
}

func TestDataplaneConformance(t *testing.T) { dataplanetest.Run(t, "memory", harness) }

func TestConcurrency(t *testing.T) { dataplanetest.Concurrency(t, harness(t)) }

func TestCustomUnits(t *testing.T) { dataplanetest.CustomUnits(t, harness(t).Store) }

func TestDowngrade(t *testing.T) { dataplanetest.Downgrade(t, harness(t).Store, nil) }

func TestStructuredOutput(t *testing.T) { dataplanetest.StructuredOutput(t, harness(t).Store) }
