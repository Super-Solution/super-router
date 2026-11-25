# SPDX-License-Identifier: Apache-2.0
"""The server entry point: configuration is refused when weak, and a memory-backed server starts and stops cleanly."""

import asyncio
import os

import httpx
import pytest

from super_router.__main__ import ConfigError, main, serve
from super_router.stores.sql import PsycopgDatabase

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


@pytest.mark.parametrize(
    "env",
    [
        {"SUPERROUTER_AUTH": "database", "SUPERROUTER_DIGEST_KEY": "d" * 32},  # needs a database
        {"SUPERROUTER_AUTH": "magic", "SUPERROUTER_API_KEY": API_KEY, "SUPERROUTER_DIGEST_KEY": "d" * 32},
    ],
)
async def test_refuses_bad_auth_configuration(env):
    with pytest.raises(ConfigError):
        await serve(env.get)


async def test_database_auth_rejects_unknown_keys():
    url = os.environ.get("DATABASE_URL", "postgresql://superrouter:local-development-only@127.0.0.1:25432/superrouter")
    try:
        probe = await asyncio.wait_for(PsycopgDatabase.connect(url), 5)
    except Exception as e:
        if os.environ.get("SR_REQUIRE_POSTGRES"):
            pytest.fail(f"postgres required but unreachable: {e}")
        pytest.skip(f"postgres unreachable: {e}")
    await probe.close()
    env = {
        "SUPERROUTER_AUTH": "database",
        "DATABASE_URL": url,
        "SUPERROUTER_DIGEST_KEY": "d" * 32,
        "ADDR": "127.0.0.1:0",
    }
    ready: asyncio.Future[tuple[str, int]] = asyncio.get_running_loop().create_future()
    stop = asyncio.Event()
    task = asyncio.create_task(serve(env.get, ready, stop))
    host, port = await asyncio.wait_for(ready, 5)
    async with httpx.AsyncClient(base_url=f"http://{host}:{port}") as c:
        # Well-formed, but no such key in the database.
        res = await c.get("/v1/services", headers={"Authorization": f"Bearer {API_KEY}"})
        assert res.status_code == 401 and res.json()["error"]["code"] == "invalid_key"
    stop.set()
    await asyncio.wait_for(task, 10)


@pytest.mark.parametrize(
    ("name", "value"),
    [
        ("OPENAI_BASE_URL", "ftp://host"),
        ("OPENAI_BASE_URL", "llm.internal"),
        ("OPENAI_BASE_URL", "https://user:secret-pass@llm.internal"),
        ("OPENAI_BASE_URL", "https://llm.internal/v1"),
        ("OPENAI_BASE_URL", "https://llm.internal/openai/v1/"),
        ("ANTHROPIC_BASE_URL", "https://llm.internal/?key=secret-pass"),
        ("ANTHROPIC_BASE_URL", "https://"),
        ("SUPERROUTER_HEALTH", "yes"),
        ("SUPERROUTER_HEALTH", "on"),  # no health book in this server: refused, never a silent no-op
    ],
)
async def test_refuses_bad_provider_options_without_echoing_them(name, value):
    env = {"SUPERROUTER_API_KEY": API_KEY, "SUPERROUTER_DIGEST_KEY": "d" * 32, "ADDR": "127.0.0.1:0", name: value}
    with pytest.raises(ConfigError) as raised:
        await serve(env.get)
    assert "secret-pass" not in str(raised.value)


def test_a_base_url_keeps_its_path_prefix_and_loses_its_trailing_slash():
    from super_router.__main__ import _base_url

    assert _base_url("OPENAI_BASE_URL", "https://llm.internal/openai/") == "https://llm.internal/openai"
    assert _base_url("OPENAI_BASE_URL", "http://127.0.0.1:9000") == "http://127.0.0.1:9000"
