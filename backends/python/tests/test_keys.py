# SPDX-License-Identifier: Apache-2.0
"""Database-backed keys against the control plane's tables (needs PostgreSQL, like test_postgres.py)."""

import asyncio
import os
import secrets
import uuid

import pytest

from super_router.pricing import RouterError
from super_router.stores.keys import PostgresKeys, hash_token, revalidate
from super_router.stores.postgres import PostgresStore
from super_router.stores.sql import PsycopgDatabase

pytestmark = pytest.mark.anyio

LOCAL_URL = "postgresql://superrouter:local-development-only@127.0.0.1:25432/superrouter"


@pytest.fixture
async def db():
    url = os.environ.get("DATABASE_URL", LOCAL_URL)
    try:
        database = await asyncio.wait_for(PsycopgDatabase.connect(url), 5)
    except Exception as e:
        if os.environ.get("SR_REQUIRE_POSTGRES"):
            pytest.fail(f"postgres required but unreachable: {e}")
        pytest.skip(f"postgres unreachable: {e}")
    await PostgresStore(database).migrate()
    yield database
    await database.close()


class Seed:
    def __init__(self, db: PsycopgDatabase) -> None:
        self.db, self.ws = db, "ws-" + str(uuid.uuid4())
        self.tokens: dict[str, tuple[str, str]] = {}

    async def run(self, sql: str, *params) -> None:
        async with self.db.transaction() as tx:
            await tx.execute(sql, *params)

    async def key(self, name: str, member: str, customer: str, scopes: list[str], services: list[str] | None) -> None:
        token, key_id = "sr_" + secrets.token_hex(32), str(uuid.uuid4())
        await self.run(
            "INSERT INTO sr_keys(workspace,environment,id,member_id,customer,hash,scopes,service_ids,expires_at)"
            " VALUES($1,'test',$2,$3,$4,$5,$6,$7,now()+interval '1 day')",
            self.ws,
            key_id,
            member,
            customer,
            hash_token(token),
            scopes,
            services,
        )
        self.tokens[name] = (token, key_id)


@pytest.fixture
async def seed(db):
    s = Seed(db)
    for member, role in (("owner1", "owner"), ("app1", "app"), ("view1", "viewer")):
        await s.run("INSERT INTO sr_members(workspace,environment,id,role) VALUES($1,'test',$2,$3)", s.ws, member, role)
    await s.run("INSERT INTO sr_customers(workspace,environment,id,external_id) VALUES($1,'test','c1','ext1')", s.ws)
    await s.key("owner", "owner1", "_admin", ["invoke", "service:write", "billing:write", "identity:write"], None)
    await s.key("app", "app1", "c1", ["invoke", "billing:write"], ["chat"])  # billing:write is beyond the app role
    return s


async def test_authenticate(db, seed):
    keys = PostgresKeys(db)
    owner = await keys.authenticate(seed.tokens["owner"][0])
    assert (owner.workspace, owner.customer, owner.member_id) == (seed.ws, "_admin", "owner1")
    assert owner.auth_key_id == seed.tokens["owner"][1] and owner.service_ids is None
    app = await keys.authenticate(seed.tokens["app"][0])
    assert app.scopes == ("invoke",) and app.service_ids == ("chat",) and app.customer == "c1"
    for bad in ("sr_" + secrets.token_hex(32), "sr_nope", ""):
        with pytest.raises(RouterError) as e:
            await keys.authenticate(bad)
        assert (e.value.code, e.value.status) == ("invalid_key", 401)


async def test_refuses_revoked_expired_and_inactive(db, seed):
    keys, token, key_id = PostgresKeys(db), seed.tokens["owner"][0], seed.tokens["owner"][1]

    async def refused() -> None:
        with pytest.raises(RouterError) as e:
            await keys.authenticate(token)
        assert e.value.code == "invalid_key"

    await seed.run("UPDATE sr_members SET active=false WHERE workspace=$1 AND id='owner1'", seed.ws)
    await refused()
    await seed.run("UPDATE sr_members SET active=true WHERE workspace=$1 AND id='owner1'", seed.ws)
    await keys.authenticate(token)
    await seed.run(
        "UPDATE sr_keys SET expires_at=now()-interval '1 second' WHERE workspace=$1 AND id=$2", seed.ws, key_id
    )
    await refused()
    await seed.run(
        "UPDATE sr_keys SET expires_at=now()+interval '1 day',revoked_at=now() WHERE workspace=$1 AND id=$2",
        seed.ws,
        key_id,
    )
    await refused()


async def test_revalidate_stops_writes_after_revocation(db, seed):
    store = PostgresStore(db, revalidate=revalidate)
    owner = await PostgresKeys(db).authenticate(seed.tokens["owner"][0])
    key_id = seed.tokens["owner"][1]
    await store.grant(owner, "customer_sale", "credit:demo", "5", "g1")

    await seed.run("UPDATE sr_keys SET scopes='{invoke}' WHERE workspace=$1 AND id=$2", seed.ws, key_id)
    with pytest.raises(RouterError) as e:
        await store.grant(owner, "customer_sale", "credit:demo", "5", "g2")
    assert e.value.code == "scope_denied"

    await seed.run("UPDATE sr_keys SET revoked_at=now() WHERE workspace=$1 AND id=$2", seed.ws, key_id)
    with pytest.raises(RouterError) as e:
        await store.grant(owner, "customer_sale", "credit:demo", "5", "g3")
    assert e.value.code == "invalid_key"

    # A principal that did not come from a key is the host's own and is not revalidated.
    from dataclasses import replace

    host = replace(owner, auth_key_id=None)
    await store.grant(host, "customer_sale", "credit:demo", "5", "g4")
