# SPDX-License-Identifier: Apache-2.0
"""Provider for the Anthropic Messages API. It streams text, normalises usage into router meters and classifies
failures like the OpenAI provider."""

from __future__ import annotations

import json
from collections.abc import AsyncIterator, Mapping
from typing import Any

import httpx

from ..contracts import Delta, Done, ProviderError, ProviderEvent, Usage, valid_meters
from ._http import KeySource, as_dict, counter, make_client, open_stream, resolve_key

_USAGE_FIELDS = ("input_tokens", "output_tokens", "cache_read_input_tokens", "cache_creation_input_tokens")


class AnthropicProvider:
    """``key`` is a string or a function read on every call, so it can rotate; an empty key is a definitive failure.
    ``base_url`` points at an Anthropic-compatible server. Pass ``client`` for proxies, tracing or tests."""

    def __init__(
        self,
        key: KeySource | None,
        base_url: str = "https://api.anthropic.com",
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
        messages = invocation["input"]["messages"]
        system = "\n".join(m["content"] for m in messages if m["role"] == "system")
        rest = [m for m in messages if m["role"] != "system"]
        if not rest:
            raise ProviderError(True, False, "messages_required")
        body = {
            "model": offering["upstream_model"],
            "max_tokens": invocation["limits"]["max_output_tokens"],
            "system": system,
            "messages": rest,
            "stream": True,
        }
        headers = {"x-api-key": key, "anthropic-version": "2023-06-01"}
        current: dict[str, int | None] | None = None
        upstream_id: str | None = None
        completion = "succeeded"
        async with open_stream(self._client, f"{self._base}/v1/messages", headers, body) as frames:
            async for frame in frames:
                try:
                    e = json.loads(frame.data)
                except ValueError:
                    raise ProviderError(False, False, "invalid_upstream_event") from None
                if not isinstance(e, dict):
                    raise ProviderError(False, False, "invalid_upstream_event")
                kind = e.get("type")
                if kind == "message_start":
                    message = as_dict(e.get("message"))
                    usage = message.get("usage")
                    current = {k: counter(usage, k) for k in _USAGE_FIELDS} if isinstance(usage, dict) else None
                    upstream_id = message.get("id") if isinstance(message.get("id"), str) else None
                elif kind == "content_block_delta":
                    delta = as_dict(e.get("delta"))
                    if delta.get("type") == "text_delta":
                        yield Delta(str(delta.get("text", "")))
                elif kind == "message_delta":
                    if current is not None:
                        # message_delta carries the final counters; they overwrite what message_start reported.
                        usage = as_dict(e.get("usage"))
                        for k in _USAGE_FIELDS:
                            if (v := counter(usage, k)) is not None:
                                current[k] = v
                        delta = as_dict(e.get("delta"))
                        if delta.get("stop_reason") == "max_tokens":
                            completion = "partial"
                elif kind == "message_stop":
                    if current is None:
                        raise ProviderError(False, False, "usage_unknown")
                    if current["input_tokens"] is None or current["output_tokens"] is None:
                        raise ProviderError(False, False, "invalid_usage")
                    meters = valid_meters(
                        {
                            "input": current["input_tokens"],
                            "cached": current["cache_read_input_tokens"] or 0,
                            "cache_write": current["cache_creation_input_tokens"] or 0,
                            "output": current["output_tokens"],
                        }
                    )
                    if meters is None:
                        raise ProviderError(False, False, "invalid_usage")
                    yield Usage(meters, upstream_id)
                    yield Done(completion)
                    return
                elif kind == "error":
                    raise ProviderError(False, False, "upstream_stream_error")
        raise ProviderError(False, False, "stream_interrupted")
