# SPDX-License-Identifier: Apache-2.0
"""Provider for the OpenAI Responses API. It streams text, normalises usage into router meters and classifies
failures: a definitive rejection (the provider certainly did no work) lets the router try the next offering,
anything uncertain keeps the reservation pending."""

from __future__ import annotations

import json
from collections.abc import AsyncIterator, Mapping
from typing import Any

import httpx

from ..contracts import Delta, Done, ProviderError, ProviderEvent, Usage, valid_meters
from ._http import KeySource, as_dict, counter, make_client, open_stream, resolve_key


class OpenAIProvider:
    """``key`` is a string or a function read on every call, so it can rotate; an empty key is a definitive failure.
    ``base_url`` points at an OpenAI-compatible server. Pass ``client`` for proxies, tracing or tests."""

    def __init__(
        self,
        key: KeySource | None,
        base_url: str = "https://api.openai.com",
        client: httpx.AsyncClient | None = None,
    ) -> None:
        self._key = key
        self._base = base_url.rstrip("/")
        self._client, self._owns = make_client(client)

    async def aclose(self) -> None:
        if self._owns:
            await self._client.aclose()

    async def invoke(self, invocation: Mapping[str, Any], offering: Mapping[str, Any]) -> AsyncIterator[ProviderEvent]:
        if invocation["input"].get("task"):
            raise ProviderError(True, False, "unsupported_task")
        key = resolve_key(self._key)
        if not key:
            raise ProviderError(True, False, "credentials_missing")
        body = {
            "model": offering["upstream_model"],
            "input": invocation["input"]["messages"],
            "max_output_tokens": invocation["limits"]["max_output_tokens"],
            "store": False,
            "stream": True,
        }
        async with open_stream(
            self._client, f"{self._base}/v1/responses", {"Authorization": f"Bearer {key}"}, body
        ) as frames:
            async for frame in frames:
                try:
                    e = json.loads(frame.data)
                except ValueError:
                    raise ProviderError(False, False, "invalid_upstream_event") from None
                if not isinstance(e, dict):
                    raise ProviderError(False, False, "invalid_upstream_event")
                kind = e.get("type")
                if kind == "response.output_text.delta":
                    yield Delta(str(e.get("delta", "")))
                elif kind in ("response.completed", "response.incomplete"):
                    response = e.get("response")
                    response = as_dict(response)
                    usage = response.get("usage")
                    if not isinstance(usage, dict):
                        raise ProviderError(False, False, "usage_unknown")
                    meters = _normalize(usage)  # raises invalid_usage for a missing or malformed counter
                    if meters is None:
                        raise ProviderError(False, False, "invalid_usage")
                    yield Usage(meters, response.get("id") if isinstance(response.get("id"), str) else None)
                    yield Done("partial" if kind == "response.incomplete" else "succeeded")
                    return
                elif kind in ("error", "response.failed"):
                    raise ProviderError(False, False, "upstream_incomplete")
        raise ProviderError(False, False, "stream_interrupted")


def _normalize(usage: Mapping[str, Any]) -> dict[str, Any] | None:
    """OpenAI counts cached and cache-write tokens inside input."""
    details = as_dict(usage.get("input_tokens_details"))
    cached, written = counter(details, "cached_tokens") or 0, counter(details, "cache_write_tokens") or 0
    return valid_meters(
        {
            "input": (counter(usage, "input_tokens", True) or 0) - cached - written,
            "cached": cached,
            "cache_write": written,
            "output": counter(usage, "output_tokens", True),
        }
    )
