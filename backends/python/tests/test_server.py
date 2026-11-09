# SPDX-License-Identifier: Apache-2.0
"""The ASGI server over real sockets (uvicorn on an ephemeral port), so streaming and disconnects are the real thing."""

import asyncio
import contextlib
import json
import time

import httpx
import pytest
import uvicorn
from dataplane_runner import DATA, DIGEST_KEY

from super_router.contracts import Principal, parse_service
from super_router.pricing import RouterError
from super_router.router import MockProvider, Router
from super_router.server import Key, StaticKeys, create_app
from super_router.stores.memory import MemoryStore

pytestmark = pytest.mark.anyio

OWNER = Principal("ws", "test", "c1", ("invoke", "service:write", "billing:write", "billing:read", "routing:write"))
READER = Principal("ws", "test", "c1", ("billing:read",))
BODY = (
    '{"service":"chat","customer_id":"c1","input":{"messages":[{"role":"user","content":"Hello"}]},'
    '"limits":{"max_output_tokens":32}%s}'
)


class Fixture:
    def __init__(self, store: MemoryStore, mock: MockProvider, url: str) -> None:
        self.store, self.mock, self.url = store, mock, url

    def client(self) -> httpx.AsyncClient:
        return httpx.AsyncClient(base_url=self.url, follow_redirects=False, timeout=10)

    async def do(self, method, path, key="", idem="", payload=None, client=None):
        headers = {}
        if key:
            headers["Authorization"] = f"Bearer {key}"
        if idem:
            headers["Idempotency-Key"] = idem
        async with client or self.client() as c:
            return await c.request(method, path, headers=headers, content=payload)


@pytest.fixture
async def make():
    stack = contextlib.AsyncExitStack()

    async def setup(behavior="success", delay=0.0) -> Fixture:
        store = MemoryStore()
        await store.publish(OWNER, parse_service(DATA["services"]["chat"]), "self-host")
        await store.grant(OWNER, "customer_sale", "credit:demo", "100000", "g1")
        mock = MockProvider(behavior, delay)
        router = Router(store, {"ok": mock}, DIGEST_KEY)
        app = create_app(router, store, StaticKeys([Key("owner-key", OWNER), Key("reader-key", READER)]))
        server = uvicorn.Server(uvicorn.Config(app, host="127.0.0.1", port=0, log_level="warning", lifespan="on"))
        task = asyncio.create_task(server.serve())
        while not server.started:
            await asyncio.sleep(0.01)
        port = server.servers[0].sockets[0].getsockname()[1]

        async def stop() -> None:
            server.should_exit = True
            await task

        stack.push_async_callback(stop)
        return Fixture(store, mock, f"http://127.0.0.1:{port}")

    async with stack:
        yield setup


def code(res: httpx.Response) -> str:
    return res.json()["error"]["code"]


async def test_inference_settles(make):
    f = await make()
    payload = BODY % ""
    res = await f.do("POST", "/v1/inferences", "owner-key", "k1", payload)
    assert res.status_code == 200 and res.headers["x-request-id"]
    out = res.json()
    assert out["billing_status"] == "settled" and out["execution_status"] == "succeeded"
    replay = await f.do("POST", "/v1/inferences", "owner-key", "k1", payload)
    assert replay.status_code == 303 and replay.headers["location"] == f"/v1/requests/{out['id']}"
    assert f.mock.calls == 1
    status = await f.do("GET", f"/v1/requests/{out['id']}", "owner-key")
    assert status.json()["billing_status"] == "settled"
    rows = (await f.do("GET", "/v1/admin/balance", "owner-key")).json()["data"]
    assert len(rows) == 1 and rows[0]["balance"] == "99965"
    services = (await f.do("GET", "/v1/services", "owner-key")).json()
    assert [s["id"] for s in services["data"]] == ["chat"] and services["next_cursor"] is None


@pytest.mark.parametrize(
    ("name", "method", "path", "key", "idem", "payload", "status", "error"),
    [
        ("no key", "POST", "/v1/inferences", "", "k", BODY % "", 401, "invalid_key"),
        ("wrong key", "POST", "/v1/inferences", "nope", "k", BODY % "", 401, "invalid_key"),
        ("no idempotency key", "POST", "/v1/inferences", "owner-key", "", BODY % "", 400, "missing_idempotency_key"),
        ("not json", "POST", "/v1/inferences", "owner-key", "k", "{", 400, "invalid_json"),
        ("NaN is not json", "POST", "/v1/inferences", "owner-key", "k", "NaN", 400, "invalid_json"),
        ("unknown field", "POST", "/v1/inferences", "owner-key", "k", BODY % ',"extra":1', 400, "invalid_request"),
        ("scope", "POST", "/v1/inferences", "reader-key", "k", BODY % "", 403, "scope_denied"),
        (
            "other customer",
            "POST",
            "/v1/inferences",
            "owner-key",
            "k",
            (BODY % "").replace('"c1"', '"c2"'),
            403,
            "scope_denied",
        ),
        (
            "unknown service",
            "POST",
            "/v1/inferences",
            "owner-key",
            "k",
            (BODY % "").replace('"chat"', '"nope"'),
            404,
            "resource_not_found",
        ),
        ("unknown route", "GET", "/v1/nothing", "owner-key", "", None, 404, "resource_not_found"),
        ("wrong method", "DELETE", "/v1/services", "owner-key", "", None, 404, "resource_not_found"),
        ("reader cannot list", "GET", "/v1/services", "reader-key", "", None, 403, "scope_denied"),
        ("balance scope", "GET", "/v1/admin/balance", "reader-key", "", None, 200, None),
        ("unknown request", "GET", "/v1/requests/nope", "owner-key", "", None, 404, "resource_not_found"),
    ],
)
async def test_errors(make, name, method, path, key, idem, payload, status, error):
    f = await make()
    res = await f.do(method, path, key, idem, payload)
    assert res.status_code == status, (name, res.text)
    if error:
        assert code(res) == error
        assert res.json()["error"]["retryable"] is False
    assert f.mock.calls == 0


async def test_body_limit(make):
    f = await make()
    huge = '{"x":"' + "a" * 256_000 + '"}'
    res = await f.do("POST", "/v1/inferences", "owner-key", "k", huge)
    assert res.status_code == 413 and code(res) == "body_too_large"


async def test_body_limit_without_content_length(make):
    f = await make()

    async def chunks():
        for _ in range(300):
            yield b"a" * 1000

    async with f.client() as c:
        res = await c.post(
            "/v1/inferences", headers={"Authorization": "Bearer owner-key", "Idempotency-Key": "k"}, content=chunks()
        )
    assert res.status_code == 413 and code(res) == "body_too_large"


async def test_streaming(make):
    f = await make()
    headers = {"Authorization": "Bearer owner-key", "Idempotency-Key": "s1"}
    async with f.client() as c:
        async with c.stream("POST", "/v1/inferences", headers=headers, content=BODY % ',"stream":true') as res:
            assert res.status_code == 200 and res.headers["content-type"] == "text/event-stream"
            assert res.headers["x-request-id"]
            text = "".join([chunk async for chunk in res.aiter_text()])
        kinds = [line[len("event: ") :] for line in text.splitlines() if line.startswith("event: ")]
        assert kinds == ["request.started", "output.delta", "request.completed"]
        # A failure before the request exists is still a JSON error with a real status.
        bad = await c.post(
            "/v1/inferences",
            headers={"Authorization": "Bearer owner-key", "Idempotency-Key": "s2"},
            content=(BODY % ',"stream":true').replace('"chat"', '"nope"'),
        )
    assert bad.status_code == 404 and bad.headers["content-type"].startswith("application/json")


async def wait_for(predicate, timeout=3.0):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if result := predicate():
            return result
        await asyncio.sleep(0.02)
    raise AssertionError("timed out")


async def test_cancel_aborts_the_provider(make):
    f = await make(delay=5)
    inference = asyncio.create_task(f.do("POST", "/v1/inferences", "owner-key", "c1", BODY % ""))
    (request_id,) = await wait_for(lambda: f.store.request_ids(OWNER))
    cancel = await f.do("POST", f"/v1/requests/{request_id}/cancel", "owner-key", "x", "{}")
    assert cancel.status_code == 202
    res = await asyncio.wait_for(inference, 3)
    # The provider reported no result, so the outcome is unknown and the funds stay held for reconciliation.
    assert "error" in res.json() or res.json().get("billing_status") != "settled"


async def test_client_disconnect_keeps_funds_held(make):
    f = await make(delay=5)
    task = asyncio.create_task(f.do("POST", "/v1/inferences", "owner-key", "d1", BODY % ""))
    (request_id,) = await wait_for(lambda: f.store.request_ids(OWNER))
    task.cancel()
    with contextlib.suppress(asyncio.CancelledError):
        await task

    async def pending():
        return (await f.store.get(OWNER, request_id)).billing == "pending"

    deadline = time.monotonic() + 3
    while time.monotonic() < deadline:
        if await pending():
            return
        await asyncio.sleep(0.02)
    raise AssertionError("a dropped client must leave the reservation pending, never released")


async def test_stream_disconnect_keeps_funds_held(make):
    f = await make(delay=5)
    headers = {"Authorization": "Bearer owner-key", "Idempotency-Key": "sd1"}

    async def consume():
        async with (
            f.client() as c,
            c.stream("POST", "/v1/inferences", headers=headers, content=BODY % ',"stream":true') as res,
        ):
            async for _ in res.aiter_bytes():
                pass

    task = asyncio.create_task(consume())
    (request_id,) = await wait_for(lambda: f.store.request_ids(OWNER))
    await asyncio.sleep(0.1)
    task.cancel()
    with contextlib.suppress(asyncio.CancelledError):
        await task
    deadline = time.monotonic() + 3
    while time.monotonic() < deadline:
        if (await f.store.get(OWNER, request_id)).billing == "pending":
            return
        await asyncio.sleep(0.02)
    raise AssertionError("a dropped stream must leave the reservation pending")


async def test_publish_and_dry_run(make):
    f = await make()
    res = await f.do("POST", "/v1/admin/routing/dry-run", "owner-key", "r1", BODY % "")
    assert res.status_code == 200
    out = res.json()
    assert len(out["candidates"]) == 1 and out["candidates"][0]["id"] == "a"
    bad = await f.do("POST", "/v1/admin/service-versions", "owner-key", "p1", '{"id":"x"}')
    assert bad.status_code == 400 and code(bad) == "invalid_request"
    assert (await f.do("GET", "/healthz")).status_code == 200
    assert (await f.do("POST", "/v1/admin/routing/dry-run", "reader-key", "r2", BODY % "")).status_code == 403


async def test_publish_new_version(make):
    f = await make()
    service = {**DATA["services"]["chat"], "version": 2}
    res = await f.do("POST", "/v1/admin/service-versions", "owner-key", "p2", json.dumps(service))
    assert res.status_code == 201 and res.json()["version"] == 2


async def test_grant_endpoint(make):
    f = await make()
    grant = '{"asset":"credit:demo","amount":"500"}'
    assert (await f.do("POST", "/v1/admin/credits/grants", "owner-key", "g-http", grant)).status_code == 201
    # The same Idempotency-Key with the same body does not fund twice; a different body conflicts.
    assert (await f.do("POST", "/v1/admin/credits/grants", "owner-key", "g-http", grant)).status_code == 201
    conflict = await f.do(
        "POST", "/v1/admin/credits/grants", "owner-key", "g-http", '{"asset":"credit:demo","amount":"501"}'
    )
    assert conflict.status_code == 409
    rows = (await f.do("GET", "/v1/admin/balance", "owner-key")).json()["data"]
    assert rows[0]["balance"] == "100500"
    for bad in (
        "{}",
        '{"asset":"credit:demo","amount":"0"}',
        '{"asset":"credit:demo","amount":"5","extra":1}',
        '{"asset":"credit:demo","amount":"5","kind":"x"}',
        '{"asset":"credit:demo","amount":5}',
        '{"asset":"credit:demo","amount":"5","expires_at":"tomorrow"}',
        "[]",
    ):
        res = await f.do("POST", "/v1/admin/credits/grants", "owner-key", "g-bad", bad)
        assert res.status_code == 400, bad
    assert (await f.do("POST", "/v1/admin/credits/grants", "reader-key", "g-r", grant)).status_code == 403
    dated = '{"asset":"credit:demo","amount":"7","expires_at":"2099-01-01T00:00:00.5Z"}'
    assert (await f.do("POST", "/v1/admin/credits/grants", "owner-key", "g-dated", dated)).status_code == 201


async def test_credit_rates_endpoint(make):
    f = await make()
    cfg = {
        "expected_version": 1,
        "config": {
            "asset": "credit:demo",
            "tokens": {k: {"units": "1000"} for k in ("input", "cached", "cache_write", "output")},
        },
    }
    res = await f.do("POST", "/v1/admin/services/chat/credit-rates", "owner-key", "cr1", json.dumps(cfg))
    assert res.status_code == 201, res.text
    assert res.json()["version"] == 2
    stale = await f.do("POST", "/v1/admin/services/chat/credit-rates", "owner-key", "cr2", json.dumps(cfg))
    assert stale.status_code == 409 and code(stale) == "service_version_conflict"
    bad = await f.do("POST", "/v1/admin/services/chat/credit-rates", "owner-key", "cr3", '{"expected_version":"2"}')
    assert bad.status_code == 400 and code(bad) == "invalid_service_version"
    junk = await f.do("POST", "/v1/admin/services/chat/credit-rates", "owner-key", "cr4", '{"x":1}')
    assert junk.status_code == 400 and code(junk) == "invalid_credit_rates"


async def test_static_keys_refuse_an_empty_token():
    auth = StaticKeys([Key("", OWNER)])
    with pytest.raises(RouterError):
        await auth.authenticate("")
