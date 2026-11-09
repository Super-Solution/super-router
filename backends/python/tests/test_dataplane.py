# SPDX-License-Identifier: Apache-2.0
"""conformance/dataplane.v1.json must hold for every DataStore: the in-memory reference here, PostgreSQL in
test_postgres.py, and the TypeScript and Go ports against their own stores."""

import time

import pytest
from dataplane_runner import Harness, run_scenario, scenarios

from super_router.contracts import Principal
from super_router.stores.memory import MemoryStore


def memory() -> Harness:
    offset = [0]

    def clock() -> int:
        return int(time.time() * 1000) + offset[0]

    store = MemoryStore(clock)

    async def advance(ms: int) -> None:
        offset[0] += ms

    async def buckets(p: Principal) -> list[dict[str, str]]:
        return sorted(store.inspect(p)["buckets"], key=lambda b: b["source"])

    return Harness(store, clock, advance, buckets)


@pytest.mark.anyio
@pytest.mark.parametrize("scenario", scenarios(), ids=lambda s: s["name"])
async def test_scenario(scenario):
    await run_scenario(scenario, memory())


def test_scenario_count():
    assert len(scenarios()) >= 25
