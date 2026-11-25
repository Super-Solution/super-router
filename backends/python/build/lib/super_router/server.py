# SPDX-License-Identifier: Apache-2.0
"""The HTTP API of the Super Router data plane as a plain ASGI 3 application.

:func:`create_app` returns an ASGI callable, so it runs on uvicorn, hypercorn and granian and mounts inside
Starlette, FastAPI (``app.mount("/router", create_app(...))``) or Django's ASGI router. It has no dependencies.
Authentication is the :class:`Authenticator` port; storage is a :class:`~super_router.router.DataStore`. Routes,
bodies and error codes match the TypeScript reference server and the Go port.
"""

from __future__ import annotations

import asyncio
import contextlib
import hashlib
import hmac
import json
import logging
import re
from collections.abc import Awaitable, Callable, MutableMapping
from dataclasses import dataclass
from datetime import datetime, timezone
from typing import Any, Protocol

from .contracts import Json, Principal, StreamEvent, parse_invocation, parse_service
from .pricing import RouterError
from .router import DataStore, Router, publish_credit_rate_version
from .routing import route

MAX_BODY_BYTES = 256_000

Scope = MutableMapping[str, Any]
Receive = Callable[[], Awaitable[MutableMapping[str, Any]]]
Send = Callable[[MutableMapping[str, Any]], Awaitable[None]]

_log = logging.getLogger("super_router.server")


class Authenticator(Protocol):
    async def authenticate(self, token: str) -> Principal:
        """Turn a bearer token into a principal, or raise ``RouterError("invalid_key", 401)``."""
        ...


@dataclass(frozen=True)
class Key:
    secret: str
    principal: Principal


class StaticKeys:
    """A fixed list of API keys for self-hosting. Digests are compared in constant time, and every key is
    checked so the time taken does not reveal which one matched."""

    def __init__(self, keys: list[Key]) -> None:
        self._keys = [(hashlib.sha256(k.secret.encode()).digest(), k.principal) for k in keys]

    async def authenticate(self, token: str) -> Principal:
        if not token:  # an empty secret (an unset variable) must not match a request with no credentials
            raise RouterError("invalid_key", 401)
        digest = hashlib.sha256(token.encode()).digest()
        found: Principal | None = None
        for stored, principal in self._keys:
            if hmac.compare_digest(stored, digest):
                found = principal
        if found is None:
            raise RouterError("invalid_key", 401)
        return found


class _ClientGone(Exception):
    pass


class _Exchange:
    """One HTTP exchange: body reading with a size cap, response writing and disconnect detection."""

    def __init__(self, receive: Receive, send: Send) -> None:
        self._receive, self._send = receive, send
        self.disconnected = asyncio.Event()
        self.started = False
        self._watcher: asyncio.Task[None] | None = None

    async def read_body(self, declared: int | None) -> bytes:
        if declared is not None and declared > MAX_BODY_BYTES:
            raise RouterError("body_too_large", 413)
        chunks: list[bytes] = []
        size = 0
        while True:
            message = await self._receive()
            if message["type"] == "http.disconnect":
                self.disconnected.set()
                raise _ClientGone
            chunk = message.get("body", b"")
            size += len(chunk)
            if size > MAX_BODY_BYTES:
                raise RouterError("body_too_large", 413)
            chunks.append(chunk)
            if not message.get("more_body"):
                break
        return b"".join(chunks)

    def watch(self) -> None:
        """Once the body is read, ``receive`` only ever reports the disconnect."""

        async def run() -> None:
            while True:
                if (await self._receive())["type"] == "http.disconnect":
                    self.disconnected.set()
                    return

        self._watcher = asyncio.create_task(run())

    async def close(self) -> None:
        if self._watcher is not None:
            self._watcher.cancel()
            await asyncio.gather(self._watcher, return_exceptions=True)

    async def start(self, status: int, headers: list[tuple[str, str]]) -> None:
        self.started = True
        await self._send(
            {
                "type": "http.response.start",
                "status": status,
                "headers": [(k.lower().encode("latin-1"), v.encode("latin-1")) for k, v in headers],
            }
        )

    async def write(self, chunk: bytes, more: bool = True) -> None:
        if self.disconnected.is_set():
            raise _ClientGone
        await self._send({"type": "http.response.body", "body": chunk, "more_body": more})

    async def json(self, status: int, value: Any, headers: list[tuple[str, str]] | None = None) -> None:
        raw = _dumps(value).encode()
        await self.start(
            status,
            [("content-type", "application/json"), ("content-length", str(len(raw))), *(headers or [])],
        )
        await self.write(raw, False)


def _dumps(value: Any) -> str:
    return json.dumps(value, separators=(",", ":"), ensure_ascii=False)


def _reject_constant(name: str) -> Any:
    raise ValueError(name)


def _parse_json(raw: bytes) -> Any:
    try:
        return json.loads(raw.decode("utf-8"), parse_constant=_reject_constant)
    except (ValueError, RecursionError):
        raise RouterError("invalid_json", 400) from None


_RFC3339 = re.compile(r"^(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d)(\.\d{1,9})?(Z|[+-]\d\d:\d\d)$")


def _epoch_ms(value: str) -> int:
    m = _RFC3339.match(value)
    if m is None:
        raise RouterError("invalid_request", 400)
    fraction = (m.group(2) or ".0")[1:7].ljust(6, "0")
    zone = "+00:00" if m.group(3) == "Z" else m.group(3)
    try:
        moment = datetime.fromisoformat(f"{m.group(1)}.{fraction}{zone}")
    except ValueError:
        raise RouterError("invalid_request", 400) from None
    return round(moment.astimezone(timezone.utc).timestamp() * 1000)


def create_app(
    router: Router,
    store: DataStore,
    auth: Authenticator,
    mode: str = "self-host",
    logger: logging.Logger | None = None,
) -> Callable[[Scope, Receive, Send], Awaitable[None]]:
    """The ASGI application. ``mode`` is "self-host" (default) or "cloud"; cloud mode forbids bring-your-own-key
    offerings and protects platform pricing from API keys."""
    log = logger or _log

    async def app(scope: Scope, receive: Receive, send: Send) -> None:
        if scope["type"] == "lifespan":
            while True:
                message = await receive()
                if message["type"] == "lifespan.startup":
                    await send({"type": "lifespan.startup.complete"})
                elif message["type"] == "lifespan.shutdown":
                    await send({"type": "lifespan.shutdown.complete"})
                    return
        if scope["type"] != "http":
            if scope["type"] == "websocket":
                await send({"type": "websocket.close", "code": 1008})
            return
        await _serve(scope, receive, send)

    async def _serve(scope: Scope, receive: Receive, send: Send) -> None:
        x = _Exchange(receive, send)
        request_id = ""
        path = (scope.get("root_path", "") and scope["path"].removeprefix(scope["root_path"])) or scope["path"]
        method = scope["method"]
        try:
            if path == "/healthz" and method == "GET":
                await x.json(200, {"status": "ok"})
                return
            headers = {k.decode("latin-1").lower(): v.decode("latin-1") for k, v in scope["headers"]}
            token = headers.get("authorization", "").removeprefix("Bearer ")
            principal = await auth.authenticate(token)
            key = headers.get("idempotency-key", "")
            if method == "POST" and not key:
                raise RouterError("missing_idempotency_key", 400)
            declared = headers.get("content-length")
            body_length = int(declared) if declared and declared.isascii() and declared.isdigit() else None
            request_id = await _route(x, method, path, principal, key, body_length)
        except _ClientGone:
            pass
        except Exception as exc:
            if x.disconnected.is_set():
                return
            await _fail(x, exc, getattr(exc, "request_id", "") or request_id)
        finally:
            await x.close()

    async def _fail(x: _Exchange, exc: Exception, request_id: str) -> None:
        error = exc if isinstance(exc, RouterError) else RouterError("internal_error", 500)
        if error.status >= 500:
            log.error("request failed code=%s request_id=%s", error.code, request_id, exc_info=exc)
        body: Json = {"code": error.code, "retryable": error.retryable}
        if request_id:
            body["request_id"] = request_id
        if error.details:
            body["details"] = dict(error.details)
        if x.started:
            return  # the response is already under way; nothing honest can be added
        with contextlib.suppress(_ClientGone):
            await x.json(error.status, {"error": body})

    async def _route(x: _Exchange, method: str, path: str, p: Principal, key: str, body_length: int | None) -> str:
        segments = path.strip("/").split("/")

        async def body() -> Any:
            raw = await x.read_body(body_length)
            x.watch()
            return _parse_json(raw)

        def need(scope_name: str) -> None:
            if not p.has_scope(scope_name):
                raise RouterError("scope_denied", 403)

        if method == "GET" and path == "/v1/services":
            need("invoke")
            data = [
                {
                    "id": s["id"],
                    "version": s["version"],
                    **({"tier": s["tier"]} if s.get("tier") else {}),
                    "max_output_tokens": s["max_output_tokens"],
                    "price": s["price"],
                }
                for s in await store.list_services(p)
            ]
            await x.json(200, {"data": data, "next_cursor": None})
        elif method == "POST" and path == "/v1/inferences":
            return await _infer(x, p, await body(), key)
        elif len(segments) in (3, 4) and segments[0] == "v1" and segments[1] == "requests":
            id = segments[2]
            if len(segments) == 4 and segments[3] == "cancel" and method == "POST":
                need("invoke")
                await store.cancel(p, id)
                await x.json(202, {"id": id, "cancel_requested": True})
            elif len(segments) == 3 and method == "GET":
                need("invoke")
                record = await store.get(p, id)
                await x.json(
                    200,
                    {
                        "id": record.id,
                        "execution_status": record.execution,
                        "billing_status": record.billing,
                        "service_version": record.service["version"],
                    },
                )
            else:
                raise RouterError("resource_not_found", 404)
        elif method == "GET" and path == "/v1/windows":
            if not (p.has_scope("credits:read") or p.has_scope("billing:read")):
                raise RouterError("scope_denied", 403)
            usage = getattr(store, "windows", None)
            if usage is None:
                raise RouterError("resource_not_found", 404)
            await x.json(200, {"windows": await usage(p)})
        elif method == "GET" and path == "/v1/admin/balance":
            need("billing:read")
            await x.json(200, {"data": [row.to_json() for row in await store.balance(p)]})
        elif (
            method == "POST"
            and len(segments) == 5
            and segments[1:3] == ["admin", "services"]
            and segments[4] == "credit-rates"
        ):
            await _credit_rates(x, p, segments[3], await body())
        elif method == "POST" and path == "/v1/admin/credits/grants":
            await _grant(x, p, await body(), key)
        elif method == "POST" and path == "/v1/admin/service-versions":
            service = parse_service(await body())
            await x.json(201, await store.publish(p, service, mode))
        elif method == "POST" and path == "/v1/admin/routing/dry-run":
            need("routing:write")
            inv = parse_invocation(await body())
            service = await store.get_service(p, inv["service"], inv.get("service_version"))
            candidates, excluded = route(service, inv)
            await x.json(
                200,
                {
                    "excluded": excluded,
                    "candidates": [{"id": c.offering["id"], "score": c.score, "cost": str(c.cost)} for c in candidates],
                },
            )
        else:
            raise RouterError("resource_not_found", 404)
        return ""

    async def _grant(x: _Exchange, p: Principal, doc: Any, key: str) -> None:
        """Fund the principal's own customer (or the platform account). The Idempotency-Key names the grant, so a
        retried request never funds twice."""
        if (
            not isinstance(doc, dict)
            or not set(doc) <= {"kind", "asset", "amount", "expires_at"}
            or not isinstance(doc.get("asset"), str)
            or not doc["asset"]
            or not isinstance(doc.get("amount"), str)
            or not doc["amount"]
            or not isinstance(doc.get("kind", ""), str)
            or not isinstance(doc.get("expires_at"), str | None)
        ):
            raise RouterError("invalid_request", 400)
        kind = doc.get("kind") or "customer_sale"
        if kind not in ("customer_sale", "platform_sale"):
            raise RouterError("invalid_request", 400)
        expires = _epoch_ms(doc["expires_at"]) if doc.get("expires_at") is not None else None
        account = await store.grant(p, kind, doc["asset"], doc["amount"], "http:" + key, expires)
        await x.json(201, {"account": account})

    async def _credit_rates(x: _Exchange, p: Principal, id: str, doc: Any) -> None:
        if not isinstance(doc, dict) or not set(doc) <= {"expected_version", "config"}:
            raise RouterError("invalid_credit_rates", 400)
        version = doc.get("expected_version")
        if not isinstance(version, int) or isinstance(version, bool):
            raise RouterError("invalid_service_version", 400)
        service = await publish_credit_rate_version(store, p, id, version, doc.get("config"), mode)
        await x.json(201, service)

    async def _infer(x: _Exchange, p: Principal, doc: Any, key: str) -> str:
        """Answer a non-streaming request with JSON, a streaming one with server-sent events. A streaming request
        only becomes an event stream once the request exists: earlier failures are ordinary JSON errors."""
        streaming = isinstance(doc, dict) and doc.get("stream") is True
        request_id = ""

        async def emit(event: StreamEvent) -> None:
            nonlocal request_id
            if x.disconnected.is_set():
                raise _ClientGone
            if not x.started:
                request_id = event.request_id
                await x.start(
                    200,
                    [
                        ("content-type", "text/event-stream"),
                        ("cache-control", "no-cache"),
                        ("x-accel-buffering", "no"),
                        ("x-request-id", request_id),
                    ],
                )
            await x.write(_sse(event.type, event.as_dict()))

        try:
            result = await router.infer(p, doc, key, emit if streaming else None, x.disconnected)
        except Exception as exc:
            if not x.started:
                raise
            if not x.disconnected.is_set():
                code = exc.code if isinstance(exc, RouterError) else "internal_error"
                failed = {"type": "request.failed", "request_id": request_id, "error": {"code": code}}
                with contextlib.suppress(_ClientGone):
                    await x.write(_sse("request.failed", failed), False)
            return request_id
        if x.started:
            await x.write(b"", False)
            return request_id
        if x.disconnected.is_set():
            return ""
        if result.get("replayed"):
            await x.start(303, [("location", f"/v1/requests/{result['id']}"), ("content-length", "0")])
            await x.write(b"", False)
        else:
            await x.json(200, result, [("x-request-id", result["id"])])
        return str(result.get("id", ""))

    return app


def _sse(kind: str, data: Json) -> bytes:
    return f"event: {kind}\ndata: {_dumps(data)}\n\n".encode()
