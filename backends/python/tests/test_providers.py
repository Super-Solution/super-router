# SPDX-License-Identifier: Apache-2.0
"""The OpenAI and Anthropic providers against fake upstreams, and the SSE reader they share."""

import json
from collections.abc import AsyncIterator, Callable

import httpx
import pytest

from super_router.contracts import Delta, Done, ProviderError, Usage
from super_router.providers import AnthropicProvider, OpenAIProvider
from super_router.providers._sse import MAX_FRAME_BYTES, FrameTooLarge, read_sse

pytestmark = pytest.mark.anyio


MESSAGES = [{"role": "system", "content": "be brief"}, {"role": "user", "content": "hi"}]


def invocation(**input_extra):
    return {"input": {"messages": MESSAGES, **input_extra}, "limits": {"max_output_tokens": 16}}


def frame(data: str) -> str:
    return f"event: x\ndata: {data}\n\n"


def stream(*frames: str, status: int = 200) -> Callable[[httpx.Request], httpx.Response]:
    return lambda request: httpx.Response(
        status, content="".join(frames).encode(), headers={"content-type": "text/event-stream"}
    )


def status(n: int) -> Callable[[httpx.Request], httpx.Response]:
    return lambda request: httpx.Response(n)


async def collect(provider, inv, offering=None):
    return [e async for e in provider.invoke(inv, offering or {"upstream_model": "m"})]


def openai(handler, key="sk-test"):
    return OpenAIProvider(key, "http://upstream", httpx.AsyncClient(transport=httpx.MockTransport(handler)))


def anthropic(handler, key="k"):
    return AnthropicProvider(key, "http://upstream", httpx.AsyncClient(transport=httpx.MockTransport(handler)))


async def failure(provider, inv=None):
    with pytest.raises(ProviderError) as e:
        await collect(provider, inv or invocation())
    return e.value.definitive, e.value.retryable, e.value.code


# --- SSE ---------------------------------------------------------------------------------------------------------


async def chunked(data: bytes, size: int) -> AsyncIterator[bytes]:
    for i in range(0, len(data), size):
        yield data[i : i + size]


@pytest.mark.parametrize("size", [1, 2, 7, 1000])
async def test_sse_frames_across_chunk_and_crlf_boundaries(size):
    data = b"event: a\r\ndata: one\r\n\r\ndata: two\ndata: lines\n\n: comment\n\nevent: last\ndata:  spaced\n\n"
    got = [(f.event, f.data) async for f in read_sse(chunked(data, size))]
    assert got == [("a", "one"), ("message", "two\nlines"), ("last", " spaced")]


async def test_sse_unterminated_frame_is_dropped():
    assert [f.data async for f in read_sse(chunked(b"data: x\n\ndata: y", 3))] == ["x"]


async def test_sse_oversized_frame():
    with pytest.raises(FrameTooLarge):
        [f async for f in read_sse(chunked(b"data: " + b"a" * (MAX_FRAME_BYTES + 1) + b"\n\n", 65536))]


async def test_sse_oversized_line_without_newline():
    with pytest.raises(FrameTooLarge):
        [f async for f in read_sse(chunked(b"a" * (MAX_FRAME_BYTES + 1), 65536))]


# --- OpenAI ------------------------------------------------------------------------------------------------------


async def test_openai_streams_text_and_normalises_usage():
    seen = {}

    def handler(request: httpx.Request) -> httpx.Response:
        seen["auth"], seen["url"], seen["body"] = (
            request.headers["authorization"],
            str(request.url),
            json.loads(request.content),
        )
        return stream(
            frame('{"type":"response.output_text.delta","delta":"Hel"}'),
            frame('{"type":"response.output_text.delta","delta":"lo"}'),
            frame(
                '{"type":"response.completed","response":{"id":"resp_1","usage":{"input_tokens":100,"output_tokens":7,'
                '"input_tokens_details":{"cached_tokens":30,"cache_write_tokens":10}}}}'
            ),
        )(request)

    events = await collect(openai(handler), invocation(), {"upstream_model": "gpt-test"})
    assert seen["auth"] == "Bearer sk-test" and seen["url"] == "http://upstream/v1/responses"
    assert seen["body"] == {
        "model": "gpt-test",
        "input": MESSAGES,
        "max_output_tokens": 16,
        "store": False,
        "stream": True,
    }
    assert events == [
        Delta("Hel"),
        Delta("lo"),
        Usage({"input": 60, "cached": 30, "cache_write": 10, "output": 7}, "resp_1"),
        Done("succeeded"),
    ]


async def test_openai_incomplete_is_partial():
    events = await collect(
        openai(
            stream(
                frame(
                    '{"type":"response.incomplete","response":{"id":"r","usage":{"input_tokens":1,"output_tokens":2}}}'
                )
            )
        ),
        invocation(),
    )
    assert events[-1] == Done("partial")


def redirect(request: httpx.Request) -> httpx.Response:
    return httpx.Response(302, headers={"location": "http://127.0.0.1:1/"})


@pytest.mark.parametrize(
    ("name", "handler", "key", "expected"),
    [
        ("rate limited", status(429), "k", (True, True, "rate_limited")),
        ("unauthorized", status(401), "k", (True, True, "credentials_rejected")),
        ("server error is unknown", status(500), "k", (False, False, "upstream_http_500")),
        ("redirect is refused", redirect, "k", (False, False, "upstream_http_302")),
        ("no credentials", status(200), "", (True, False, "credentials_missing")),
        (
            "transport failure",
            lambda r: (_ for _ in ()).throw(httpx.ConnectError("boom")),
            "k",
            (False, False, "transport_unknown"),
        ),
        (
            "truncated stream",
            stream(frame('{"type":"response.output_text.delta","delta":"a"}')),
            "k",
            (False, False, "stream_interrupted"),
        ),
        (
            "unterminated frame is dropped",
            stream('data: {"type":"response.completed"}'),
            "k",
            (False, False, "stream_interrupted"),
        ),
        ("bad event json", stream(frame("not json")), "k", (False, False, "invalid_upstream_event")),
        ("non-object event", stream(frame("[1]")), "k", (False, False, "invalid_upstream_event")),
        (
            "completed without usage",
            stream(frame('{"type":"response.completed","response":{"id":"r"}}')),
            "k",
            (False, False, "usage_unknown"),
        ),
        (
            "negative input after cache",
            stream(
                frame(
                    '{"type":"response.completed","response":{"id":"r","usage":{"input_tokens":1,"output_tokens":1,'
                    '"input_tokens_details":{"cached_tokens":5}}}}'
                )
            ),
            "k",
            (False, False, "invalid_usage"),
        ),
        ("upstream error event", stream(frame('{"type":"error"}')), "k", (False, False, "upstream_incomplete")),
    ],
)
async def test_openai_failures(name, handler, key, expected):
    assert await failure(openai(handler, key)) == expected, name


async def test_openai_tasks_are_unsupported():
    task = invocation(task={"kind": "image", "prompt": "x"})
    assert await failure(openai(status(200)), task) == (True, False, "unsupported_task")


async def test_openai_key_is_read_on_every_call():
    keys = iter(["one", "two"])
    seen = []

    def handler(request: httpx.Request) -> httpx.Response:
        seen.append(request.headers["authorization"])
        return httpx.Response(401)

    provider = openai(handler, lambda: next(keys))
    for _ in range(2):
        await failure(provider)
    assert seen == ["Bearer one", "Bearer two"]


async def test_openai_stops_when_the_caller_stops():
    provider = openai(
        stream(
            frame('{"type":"response.output_text.delta","delta":"a"}'),
            frame('{"type":"response.output_text.delta","delta":"b"}'),
        )
    )
    events = provider.invoke(invocation(), {"upstream_model": "m"})
    assert await anext(events) == Delta("a")
    await events.aclose()  # must release the connection without raising


# --- Anthropic ---------------------------------------------------------------------------------------------------

START = frame('{"type":"message_start","message":{"id":"m","usage":{"input_tokens":1,"output_tokens":1}}}')


async def test_anthropic_streams_text_and_merges_usage():
    seen = {}

    def handler(request: httpx.Request) -> httpx.Response:
        seen["headers"], seen["url"], seen["body"] = request.headers, str(request.url), json.loads(request.content)
        return stream(
            frame(
                '{"type":"message_start","message":{"id":"msg_1","usage":{"input_tokens":50,"cache_read_input_tokens":20,'
                '"cache_creation_input_tokens":5,"output_tokens":1,"cache_creation":{"ephemeral_5m_input_tokens":5}}}}'
            ),
            frame('{"type":"content_block_delta","delta":{"type":"text_delta","text":"Hi"}}'),
            frame('{"type":"content_block_delta","delta":{"type":"input_json_delta","partial_json":"{}"}}'),
            frame(
                '{"type":"message_delta","delta":{"stop_reason":"end_turn"},'
                '"usage":{"output_tokens":9,"server_tool_use":{"web_search_requests":0}}}'
            ),
            frame('{"type":"message_stop"}'),
        )(request)

    events = await collect(anthropic(handler, "key"), invocation(), {"upstream_model": "claude-test"})
    assert seen["headers"]["x-api-key"] == "key" and seen["headers"]["anthropic-version"] == "2023-06-01"
    assert seen["url"] == "http://upstream/v1/messages"
    assert seen["body"] == {
        "model": "claude-test",
        "max_tokens": 16,
        "system": "be brief",
        "messages": [{"role": "user", "content": "hi"}],
        "stream": True,
    }
    assert events == [
        Delta("Hi"),
        Usage({"input": 50, "cached": 20, "cache_write": 5, "output": 9}, "msg_1"),
        Done("succeeded"),
    ]


@pytest.mark.parametrize(
    ("cache", "sent"),
    [
        (None, False),
        ({"mode": "none"}, False),
        ({"mode": "auto"}, True),
    ],
)
async def test_anthropic_automatic_caching_is_sent_only_when_asked_for(cache, sent):
    seen = {}

    def handler(request: httpx.Request) -> httpx.Response:
        seen["body"] = json.loads(request.content)
        return stream(START, frame('{"type":"message_stop"}'))(request)

    inv = invocation()
    if cache is not None:
        inv["cache"] = cache
    await collect(anthropic(handler), inv)
    assert seen["body"].get("cache_control") == ({"type": "ephemeral"} if sent else None)


RESPONSE_FORMAT = {
    "type": "json_schema",
    "json_schema": {
        "name": "flag",
        "description": "A flag",
        "schema": {"type": "object", "properties": {"ok": {"type": "boolean"}}, "required": ["ok"]},
        "strict": True,
    },
}


async def test_structured_output_is_declared_and_written_the_way_each_api_takes_it():
    assert "structured_output" in OpenAIProvider.capabilities
    assert "structured_output" in AnthropicProvider.capabilities
    schema = RESPONSE_FORMAT["json_schema"]["schema"]
    seen = {}

    def capture(name):
        def handler(request: httpx.Request) -> httpx.Response:
            seen[name] = json.loads(request.content)
            return stream(START, frame('{"type":"message_stop"}'))(request)

        return handler

    inv = invocation()
    inv["response_format"] = RESPONSE_FORMAT
    await collect(anthropic(capture("anthropic")), inv)
    assert seen["anthropic"]["output_config"] == {"format": {"type": "json_schema", "schema": schema}}
    with pytest.raises(ProviderError):  # the canned reply is not an OpenAI stream; the request is what matters
        await collect(openai(capture("openai")), inv)
    assert seen["openai"]["text"] == {
        "format": {"type": "json_schema", "name": "flag", "description": "A flag", "schema": schema, "strict": True}
    }
    plain = invocation()
    await collect(anthropic(capture("plain")), plain)
    assert "output_config" not in seen["plain"]


async def test_anthropic_max_tokens_is_partial():
    events = await collect(
        anthropic(
            stream(
                START,
                frame('{"type":"message_delta","delta":{"stop_reason":"max_tokens"},"usage":{"output_tokens":16}}'),
                frame('{"type":"message_stop"}'),
            )
        ),
        invocation(),
    )
    assert events[-1] == Done("partial")


@pytest.mark.parametrize(
    ("name", "handler", "key", "expected"),
    [
        ("rate limited", status(429), "k", (True, True, "rate_limited")),
        ("bad request", status(400), "k", (True, False, "upstream_http_400")),
        ("overloaded is unknown", status(529), "k", (False, False, "upstream_http_529")),
        ("no credentials", status(200), "", (True, False, "credentials_missing")),
        ("truncated stream", stream(START), "k", (False, False, "stream_interrupted")),
        ("stop without start", stream(frame('{"type":"message_stop"}')), "k", (False, False, "usage_unknown")),
        ("error event", stream(START, frame('{"type":"error"}')), "k", (False, False, "upstream_stream_error")),
        ("bad event json", stream(frame("{")), "k", (False, False, "invalid_upstream_event")),
        (
            "start without counters is not free",
            stream(frame('{"type":"message_start","message":{"id":"m","usage":{}}}'), frame('{"type":"message_stop"}')),
            "k",
            (False, False, "invalid_usage"),
        ),
        (
            "start without output",
            stream(
                frame('{"type":"message_start","message":{"id":"m","usage":{"input_tokens":3}}}'),
                frame('{"type":"message_stop"}'),
            ),
            "k",
            (False, False, "invalid_usage"),
        ),
        (
            "non-integer counter",
            stream(START, frame('{"type":"message_delta","delta":{},"usage":{"output_tokens":"9"}}')),
            "k",
            (False, False, "invalid_usage"),
        ),
    ],
)
async def test_anthropic_failures(name, handler, key, expected):
    assert await failure(anthropic(handler, key)) == expected, name


async def test_anthropic_tasks_and_system_only_are_definitive_failures():
    assert await failure(anthropic(status(200)), invocation(task={"kind": "image", "prompt": "x"})) == (
        True,
        False,
        "unsupported_task",
    )
    only_system = {"input": {"messages": [{"role": "system", "content": "x"}]}, "limits": {"max_output_tokens": 16}}
    assert await failure(anthropic(status(200)), only_system) == (True, False, "messages_required")


async def test_anthropic_delta_may_complete_the_counters():
    events = await collect(
        anthropic(
            stream(
                frame('{"type":"message_start","message":{"id":"m","usage":{"input_tokens":7}}}'),
                frame('{"type":"message_delta","delta":{},"usage":{"output_tokens":4}}'),
                frame('{"type":"message_stop"}'),
            )
        ),
        invocation(),
    )
    assert events[0] == Usage({"input": 7, "cached": 0, "cache_write": 0, "output": 4}, "m")
