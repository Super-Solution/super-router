# SPDX-License-Identifier: Apache-2.0
"""What the HTTP providers share: key lookup, request, failure classification and the event loop around SSE."""

from __future__ import annotations

from collections.abc import AsyncIterator, Callable, Mapping
from contextlib import asynccontextmanager
from typing import Any

import httpx

from ..contracts import ProviderError
from ._sse import Frame, FrameTooLarge, read_sse

KeySource = str | Callable[[], str]


def as_dict(value: Any) -> dict[str, Any]:
    return value if isinstance(value, dict) else {}


def counter(source: Mapping[str, Any], key: str, required: bool = False) -> int | None:
    """A token counter from provider usage. A missing optional counter is None; a missing required one, or any present
    value that is not a non-negative integer, is an invalid usage record. Never default a required counter to zero:
    that would bill a response with unreadable usage as free."""
    value = source.get(key)
    if value is None:
        if required:
            raise ProviderError(False, False, "invalid_usage")
        return None
    if not isinstance(value, int) or isinstance(value, bool) or value < 0:
        raise ProviderError(False, False, "invalid_usage")
    return value


def resolve_key(key: KeySource | None) -> str:
    value = key() if callable(key) else key
    return value or ""


def make_client(client: httpx.AsyncClient | None) -> tuple[httpx.AsyncClient, bool]:
    """The client to use and whether the provider owns it. Redirects are refused on a client we create."""
    if client is not None:
        return client, False
    return httpx.AsyncClient(follow_redirects=False, timeout=httpx.Timeout(None, connect=10)), True


@asynccontextmanager
async def open_stream(
    client: httpx.AsyncClient, url: str, headers: dict[str, str], body: dict[str, Any]
) -> AsyncIterator[AsyncIterator[Frame]]:
    """POST and yield the event stream, or raise the ProviderError the status deserves.

    A definitive status (400, 401, 403, 404, 422, 429) means the provider did no work; 429 may be retried on another
    offering. Everything else, including a redirect (never followed) and a lost connection, is an unknown outcome."""
    try:
        request = client.build_request("POST", url, headers={**headers, "Content-Type": "application/json"}, json=body)
        response = await client.send(request, stream=True, follow_redirects=False)
    except httpx.HTTPError:
        raise ProviderError(False, False, "transport_unknown") from None
    try:
        status = response.status_code
        if not 200 <= status <= 299:
            if status in (400, 401, 403, 404, 422):
                raise ProviderError(True, False, f"upstream_http_{status}")
            if status == 429:
                raise ProviderError(True, True, "upstream_http_429")
            raise ProviderError(False, False, f"upstream_http_{status}")
        yield _frames(response)
    finally:
        await response.aclose()


async def _frames(response: httpx.Response) -> AsyncIterator[Frame]:
    try:
        async for frame in read_sse(response.aiter_bytes()):
            yield frame
    except FrameTooLarge:
        raise ProviderError(False, False, "stream_frame_too_large") from None
    except httpx.HTTPError:
        raise ProviderError(False, False, "stream_interrupted") from None
