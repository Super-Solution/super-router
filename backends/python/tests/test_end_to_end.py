# SPDX-License-Identifier: Apache-2.0
"""Everything wired together: ASGI server -> router -> OpenAI provider -> a fake upstream over real sockets."""

import asyncio
import contextlib
import json

import httpx
import pytest
import uvicorn
from dataplane_runner import DATA, DIGEST_KEY

from super_router.contracts import Principal
from super_router.providers import OpenAIProvider
from super_router.router import Router
from super_router.server import Key, StaticKeys, create_app
from super_router.stores.memory import MemoryStore

pytestmark = pytest.mark.anyio

OWNER = Principal("ws", "test", "c1", ("invoke", "service:write", "billing:write", "billing:read"))


def fake_openai(seen: list[dict]):
    async def app(scope, receive, send):
        if scope["type"] != "http":
            return
        body = b""
        while True:
            message = await receive()
            body += message.get("body", b"")
            if not message.get("more_body"):
                break
        seen.append({"auth": dict(scope["headers"])[b"authorization"].decode(), "body": json.loads(body)})
        events = [
            {"type": "response.output_text.delta", "delta": "Hel"},
            {"type": "response.output_text.delta", "delta": "lo"},
            {
                "type": "response.completed",
                "response": {"id": "resp_1", "usage": {"input_tokens": 10, "output_tokens": 5}},
            },
        ]
        await send({"type": "http.response.start", "status": 200, "headers": [(b"content-type", b"text/event-stream")]})
        for e in events:
            await send({"type": "http.response.body", "body": f"data: {json.dumps(e)}\n\n".encode(), "more_body": True})
            await asyncio.sleep(0.01)
        await send({"type": "http.response.body", "body": b"", "more_body": False})

    return app


@contextlib.asynccontextmanager
async def serving(app):
    server = uvicorn.Server(uvicorn.Config(app, host="127.0.0.1", port=0, log_level="warning"))
    task = asyncio.create_task(server.serve())
    while not server.started:
        await asyncio.sleep(0.01)
    try:
        yield f"http://127.0.0.1:{server.servers[0].sockets[0].getsockname()[1]}"
    finally:
        server.should_exit = True
        await task


async def test_publish_fund_and_stream_through_a_real_provider():
    seen: list[dict] = []
    async with serving(fake_openai(seen)) as upstream:
        store = MemoryStore()
        provider = OpenAIProvider("sk-upstream", upstream)
        router = Router(store, {"openai": provider}, DIGEST_KEY)
        api = create_app(router, store, StaticKeys([Key("owner-key", OWNER)]))
        async with serving(api) as base, httpx.AsyncClient(base_url=base, timeout=10) as c:
            h = {"Authorization": "Bearer owner-key"}
            service = json.loads(json.dumps(DATA["services"]["chat"]))
            service["offerings"][0]["provider"] = "openai"
            assert (
                await c.post("/v1/admin/service-versions", headers={**h, "Idempotency-Key": "p"}, json=service)
            ).status_code == 201
            grant = {"asset": "credit:demo", "amount": "100000"}
            assert (
                await c.post("/v1/admin/credits/grants", headers={**h, "Idempotency-Key": "g"}, json=grant)
            ).status_code == 201
            request = {
                "service": "chat",
                "customer_id": "c1",
                "input": {"messages": [{"role": "user", "content": "Hello"}]},
                "limits": {"max_output_tokens": 32},
                "stream": True,
            }
            async with c.stream("POST", "/v1/inferences", headers={**h, "Idempotency-Key": "i1"}, json=request) as res:
                text = "".join([chunk async for chunk in res.aiter_text()])
            events = [json.loads(line[6:]) for line in text.splitlines() if line.startswith("data: ")]
            assert [e["type"] for e in events] == [
                "request.started",
                "output.delta",
                "output.delta",
                "request.completed",
            ]
            assert "".join(e["text"] for e in events if e["type"] == "output.delta") == "Hello"
            # 10 input at 2 + 5 output at 3 customer credits.
            rows = (await c.get("/v1/admin/balance", headers=h)).json()["data"]
            assert rows[0]["balance"] == str(100000 - 35)
    await provider.aclose()
    assert seen[0]["auth"] == "Bearer sk-upstream"
    assert seen[0]["body"]["model"] == "mock-model" and seen[0]["body"]["store"] is False
