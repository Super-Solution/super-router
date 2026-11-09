# SPDX-License-Identifier: Apache-2.0
"""The same data-plane conformance, plus concurrency invariants, on a real PostgreSQL.

Set DATABASE_URL, or run the local development database on 127.0.0.1:25432. The tests skip when it is not
reachable; SR_REQUIRE_POSTGRES=1 turns the skip into a failure, as CI should.
"""

import asyncio
import os
import time
import uuid
from pathlib import Path

import pytest
from dataplane_runner import DATA, DIGEST_KEY, Harness, run_scenario, scenarios

from super_router.contracts import Principal, parse_service
from super_router.pricing import RouterError
from super_router.router import MockProvider, Router
from super_router.stores.postgres import PostgresStore
from super_router.stores.sql import PsycopgDatabase

LOCAL_URL = "postgresql://superrouter:local-development-only@127.0.0.1:25432/superrouter"
ROOT = Path(__file__).resolve().parents[3]


@pytest.fixture
async def store():
    url = os.environ.get("DATABASE_URL", LOCAL_URL)
    try:
        db = await asyncio.wait_for(PsycopgDatabase.connect(url, max_size=32), 5)
    except Exception as e:
        if os.environ.get("SR_REQUIRE_POSTGRES"):
            pytest.fail(f"postgres required but unreachable: {e}")
        pytest.skip(f"postgres unreachable (set DATABASE_URL): {e}")
    s = PostgresStore(db)
    await s.migrate()
    yield s
    await db.close()


def clock() -> int:
    return int(time.time() * 1000)


@pytest.mark.anyio
@pytest.mark.parametrize("scenario", scenarios(), ids=lambda s: s["name"])
async def test_scenario(store, scenario):
    # Recovery is global: settle what earlier runs left expired so a scenario counts only its own.
    await store.recover()

    async def advance(ms: int) -> None:
        await asyncio.sleep(ms / 1000)

    await run_scenario(scenario, Harness(store, clock, advance, store.buckets))


@pytest.mark.anyio
async def test_migrate_is_idempotent(store):
    await store.migrate()


def test_migrations_match_typescript():
    theirs = sorted((ROOT / "packages" / "adapters" / "storage-postgres" / "migrations").glob("*.sql"))
    ours = sorted((ROOT / "backends" / "python" / "src" / "super_router" / "stores" / "migrations").glob("*.sql"))
    assert theirs, "TypeScript migrations not found"
    assert [p.name for p in ours] == [p.name for p in theirs]
    for a, b in zip(theirs, ours, strict=True):
        assert a.read_bytes() == b.read_bytes(), f"{b.name} differs from the TypeScript store"


@pytest.mark.anyio
async def test_concurrency(store):
    """More simultaneous requests than the balance holds: no overspend, nothing left held, refusals are 402."""
    service = parse_service(DATA["services"]["chat"])
    p = Principal(
        "ws-" + str(uuid.uuid4()),
        "test",
        "c1",
        ("invoke", "service:write", "billing:write", "billing:read"),
    )
    await store.publish(p, service, "self-host")
    hold, cost, workers = 2144, 35, 24  # one request reserves 2144 atoms and costs 35 once settled
    await store.grant(p, "customer_sale", "credit:demo", str(hold * 10), "g1", None)
    mock = MockProvider("success", delay=0.4)
    router = Router(store, {"ok": mock}, DIGEST_KEY, clock)

    results = await asyncio.gather(
        *(router.infer(p, DATA["invocation"], f"key-{i}") for i in range(workers)), return_exceptions=True
    )
    settled = [r for r in results if not isinstance(r, BaseException)]
    refused = [r for r in results if isinstance(r, RouterError) and r.code == "insufficient_balance"]
    assert all(r.status == 402 for r in refused)
    assert len(settled) >= 10 and refused and len(settled) + len(refused) == workers, results
    assert mock.calls == len(settled)
    rows = await store.balance(p)
    assert len(rows) == 1
    assert rows[0].balance == str(hold * 10 - cost * len(settled)) and rows[0].held == "0"
