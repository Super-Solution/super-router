# SPDX-License-Identifier: Apache-2.0
"""Session + weekly usage windows on subscription credits, on a real PostgreSQL (docs/WINDOW_LIMITS.md).

One chat call reserves 2144 atoms and the mock settles it at 35. ``FOUR`` leaves room for exactly four
reservations: 35k + 2144 <= 2250 holds for k = 0..3.
"""

import asyncio
import os
import time
import uuid
from typing import Any

import pytest
from dataplane_runner import DATA, DIGEST_KEY

from super_router.contracts import Principal, parse_service
from super_router.pricing import RouterError
from super_router.router import MockProvider, Router
from super_router.stores.postgres import PostgresStore
from super_router.stores.sql import PsycopgDatabase

LOCAL_URL = "postgresql://superrouter:local-development-only@127.0.0.1:25432/superrouter"
CHARGE = 35
FOUR = "2250"


def clock() -> int:
    return int(time.time() * 1000)


@pytest.fixture
async def db():
    url = os.environ.get("DATABASE_URL", LOCAL_URL)
    try:
        database = await asyncio.wait_for(PsycopgDatabase.connect(url, max_size=32), 5)
    except Exception as e:
        if os.environ.get("SR_REQUIRE_POSTGRES"):
            pytest.fail(f"postgres required but unreachable: {e}")
        pytest.skip(f"postgres unreachable (set DATABASE_URL): {e}")
    await PostgresStore(database).migrate()
    yield database
    await database.close()


class Fixture:
    def __init__(self, db: Any, store: PostgresStore, p: Principal, router: Router) -> None:
        self.db, self.store, self.p, self.router = db, store, p, router
        self.n = 0

    async def call(self, router: Router | None = None) -> Any:
        self.n += 1
        return await (router or self.router).infer(self.p, DATA["invocation"], f"k-{self.n}-{uuid.uuid4()}")

    async def refused(self) -> RouterError:
        with pytest.raises(RouterError) as e:
            await self.call()
        return e.value

    async def sql(self, query: str, *params: Any) -> list[Any]:
        async with self.db.transaction() as tx:
            return await tx.fetch_all(query, self.p.workspace, *params)

    async def rows(self) -> int:
        return int((await self.sql("SELECT count(*)::int AS n FROM sr_usage_windows WHERE workspace=$1"))[0]["n"])

    async def window(self, kind: str) -> dict[str, Any]:
        return next(w for w in await self.store.windows(self.p) if w["window"] == kind)

    async def age(self, kind: str, hours: int) -> None:
        async with self.db.transaction() as tx:
            await tx.execute(
                "UPDATE sr_usage_windows SET window_start=window_start-make_interval(hours=>$3),"
                "window_end=window_end-make_interval(hours=>$3) WHERE workspace=$1 AND kind=$2",
                self.p.workspace,
                kind,
                hours,
            )


async def fixture(db: Any, limits: dict[str, Any] | None, behavior: str = "success") -> Fixture:
    """Publish the conformance chat service and fund the customer through a 100000-credit subscription bucket
    whose entitlement carries ``limits`` (None: the option is off)."""
    store = PostgresStore(db)
    p = Principal(
        "win-" + str(uuid.uuid4()),
        "test",
        "c1",
        ("invoke", "service:write", "billing:write", "billing:read", "credits:read"),
    )
    await store.publish(p, parse_service(DATA["services"]["chat"]), "self-host")
    async with db.transaction() as tx:
        for q in (
            "INSERT INTO sr_customers(workspace,environment,id,external_id) VALUES($1,$2,'c1','c1')",
            "INSERT INTO sr_plans(workspace,environment,id,version,config) VALUES($1,$2,'plan',1,'{}')",
            "INSERT INTO sr_subscriptions(workspace,environment,id,customer,plan_id,plan_version,anchor_at,"
            "period_days,status) VALUES($1,$2,'sub','c1','plan',1,now(),30,'active')",
        ):
            await tx.execute(q, p.workspace, p.environment)
    now = clock()
    end = now + 30 * 86_400_000
    await store.grant(
        p, "customer_sale", "credit:demo", "100000", "subscription:sub:0", end, ["chat"], source_kind="subscription"
    )
    bucket = (await store.buckets(p))[0]
    async with db.transaction() as tx:
        bucket_id = (
            await tx.fetch_one(
                "SELECT id FROM sr_buckets WHERE workspace=$1 AND environment=$2 AND source='subscription:sub:0'",
                p.workspace,
                p.environment,
            )
        )["id"]
    assert bucket["source"] == "subscription:sub:0"
    await store.entitle(p, "sub", 0, ["chat"], now - 3_600_000, end, bucket_id, limits)
    router = Router(store, {"ok": MockProvider(behavior)}, DIGEST_KEY, clock)
    return Fixture(db, store, p, router)


@pytest.mark.anyio
@pytest.mark.parametrize("limits", [None, {"enabled": False, "session": {"hours": 5, "credits": "1"}}])
async def test_off_by_default(db, limits):
    f = await fixture(db, limits)
    for _ in range(3):
        await f.call()
    assert await f.rows() == 0


@pytest.mark.anyio
async def test_session_window_admits_what_fits_then_refuses(db):
    f = await fixture(db, {"enabled": True, "session": {"hours": 5, "credits": FOUR}})
    for _ in range(4):
        await f.call()
    w = await f.window("session")
    assert (w["spent"], w["held"], w["limit"], w["hours"]) == (str(CHARGE * 4), "0", FOUR, 5)
    e = await f.refused()
    assert (e.code, e.status, e.retryable) == ("window_limit_reached", 429, True)
    assert e.details is not None
    assert e.details["window"] == "session"
    assert (e.details["limit"], e.details["used"]) == (FOUR, str(CHARGE * 4))
    assert e.details["resets_at"] == w["resets_at"]
    held = (await f.sql("SELECT held::text AS held FROM sr_accounts WHERE workspace=$1"))[0]["held"]
    assert held == "0"  # the refusal reserved nothing
    await f.age("session", 6)
    await f.call()
    assert (await f.window("session"))["spent"] == str(CHARGE)


@pytest.mark.anyio
async def test_weekly_window_binds_independently(db):
    # The session fits four calls; the week fits eight (35k + 2144 <= 2400 for k = 0..7).
    f = await fixture(db, {"enabled": True, "session": {"hours": 5, "credits": FOUR}, "weekly": {"credits": "2400"}})
    for _ in range(4):
        await f.call()
    assert (await f.refused()).details["window"] == "session"  # type: ignore[index]
    await f.age("session", 6)
    for _ in range(4):
        await f.call()
    await f.age("session", 6)
    assert (await f.refused()).details["window"] == "weekly"  # type: ignore[index]
    await f.age("weekly", 169)
    await f.call()


@pytest.mark.anyio
async def test_limit_below_one_reservation_is_not_retryable(db):
    f = await fixture(db, {"enabled": True, "session": {"hours": 5, "credits": "1000"}})
    e = await f.refused()
    assert e.code == "window_limit_reached" and not e.retryable
    assert e.details is not None and e.details["resets_at"] is None
    assert await f.rows() == 0


@pytest.mark.anyio
async def test_full_window_falls_through_to_purchased_credits(db):
    f = await fixture(db, {"enabled": True, "session": {"hours": 5, "credits": FOUR}})
    await f.store.grant(f.p, "customer_sale", "credit:demo", "100000", "pack", None)
    # 75 calls cost 2625: the window takes exactly its 2250 and the pack the other 375, including the call whose
    # hold straddles both buckets.
    for _ in range(75):
        await f.call()
    w = await f.window("session")
    assert (w["spent"], w["held"]) == ("2250", "0")
    remaining = {b["source"]: (b["remaining"], b["held"]) for b in await f.store.buckets(f.p)}
    assert remaining == {"subscription:sub:0": ("97750", "0"), "pack": ("99625", "0")}


@pytest.mark.anyio
async def test_released_request_does_not_start_the_clock(db):
    f = await fixture(db, {"enabled": True, "session": {"hours": 5, "credits": FOUR}}, "reject")
    result = await f.call()
    assert result["billing_status"] == "released"
    assert await f.rows() == 0


@pytest.mark.anyio
async def test_concurrent_calls_cannot_overshoot_a_window(db):
    f = await fixture(db, {"enabled": True, "session": {"hours": 5, "credits": FOUR}})
    results = await asyncio.gather(*(f.call() for _ in range(12)), return_exceptions=True)
    ok = [r for r in results if not isinstance(r, BaseException)]
    assert 1 <= len(ok) <= 4
    for r in results:
        if isinstance(r, BaseException):
            assert isinstance(r, RouterError) and r.code == "window_limit_reached", r
    w = await f.window("session")
    assert (w["spent"], w["held"]) == (str(CHARGE * len(ok)), "0")
