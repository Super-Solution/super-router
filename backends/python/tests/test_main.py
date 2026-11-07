# SPDX-License-Identifier: Apache-2.0
"""The server entry point: configuration is refused when weak, and a memory-backed server starts and stops cleanly."""

import asyncio

import httpx
import pytest

from super_router.__main__ import ConfigError, main, serve

pytestmark = pytest.mark.anyio

API_KEY = "sr_" + "0123456789abcdef" * 4


@pytest.mark.parametrize(
    "env",
    [
        {},
        {"SUPERROUTER_API_KEY": "short", "SUPERROUTER_DIGEST_KEY": "d" * 32},
        {"SUPERROUTER_API_KEY": API_KEY, "SUPERROUTER_DIGEST_KEY": "short"},
        {"SUPERROUTER_API_KEY": API_KEY, "SUPERROUTER_DIGEST_KEY": "d" * 32, "ADDR": "nonsense"},
    ],
)
async def test_refuses_weak_configuration(env):
    with pytest.raises(ConfigError):
        await serve(env.get)


def test_main_exits_nonzero_on_bad_configuration():
    assert main([], {}) == 1


async def test_serves_through_the_memory_store_and_shuts_down_cleanly():
    env = {
        "SUPERROUTER_API_KEY": API_KEY,
        "SUPERROUTER_DIGEST_KEY": "d" * 32,
        "ADDR": "127.0.0.1:0",
        "SUPERROUTER_MOCK": "1",
    }
    ready: asyncio.Future[tuple[str, int]] = asyncio.get_running_loop().create_future()
    stop = asyncio.Event()
    task = asyncio.create_task(serve(env.get, ready, stop))
    host, port = await asyncio.wait_for(ready, 5)
    headers = {"Authorization": f"Bearer {API_KEY}"}
    async with httpx.AsyncClient(base_url=f"http://{host}:{port}") as c:
        assert (await c.get("/healthz")).status_code == 200
        assert (await c.get("/v1/services", headers=headers)).status_code == 200
        missing = await c.get("/v1/requests/x", headers=headers)
        assert missing.status_code == 404 and missing.json()["error"]["code"] == "resource_not_found"
        assert (await c.get("/v1/services")).status_code == 401
    stop.set()
    await asyncio.wait_for(task, 10)
