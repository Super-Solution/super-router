# SPDX-License-Identifier: Apache-2.0
"""The app mounts under a prefix in Starlette and FastAPI, which pass the prefix as ``root_path``."""

import httpx
import pytest
from dataplane_runner import DATA, DIGEST_KEY

from super_router.contracts import Principal, parse_service
from super_router.router import MockProvider, Router
from super_router.server import Key, StaticKeys, create_app
from super_router.stores.memory import MemoryStore

pytestmark = pytest.mark.anyio

OWNER = Principal("ws", "test", "c1", ("invoke", "service:write", "billing:write", "billing:read"))
BODY = {
    "service": "chat",
    "customer_id": "c1",
    "input": {"messages": [{"role": "user", "content": "Hello"}]},
    "limits": {"max_output_tokens": 32},
}


async def router_app() -> object:
    store = MemoryStore()
    await store.publish(OWNER, parse_service(DATA["services"]["chat"]), "self-host")
    await store.grant(OWNER, "customer_sale", "credit:demo", "100000", "g1")
    router = Router(store, {"ok": MockProvider()}, DIGEST_KEY)
    return create_app(router, store, StaticKeys([Key("owner-key", OWNER)]))


@pytest.mark.parametrize("framework", ["starlette", "fastapi"])
async def test_mounted_under_a_prefix(framework):
    pytest.importorskip(framework)
    api = await router_app()
    if framework == "starlette":
        from starlette.applications import Starlette
        from starlette.routing import Mount

        host = Starlette(routes=[Mount("/router", app=api)])
    else:
        from fastapi import FastAPI

        host = FastAPI()
        host.mount("/router", api)
    headers = {"Authorization": "Bearer owner-key", "Idempotency-Key": "k1"}
    async with httpx.AsyncClient(transport=httpx.ASGITransport(app=host), base_url="http://test") as c:
        assert (await c.get("/router/healthz")).status_code == 200
        res = await c.post("/router/v1/inferences", headers=headers, json=BODY)
        assert res.status_code == 200 and res.json()["billing_status"] == "settled"
        assert (await c.get("/router/v1/nothing", headers=headers)).status_code == 404
