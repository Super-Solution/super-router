# SPDX-License-Identifier: Apache-2.0
"""conformance/failure.v1.json and retry_after.v1.json: an upstream refusal to its class, error and wait, and the same
refusals served as real HTTP responses to the OpenAI and Anthropic providers."""

import json
from pathlib import Path

import httpx
import pytest

from super_router.contracts import ProviderError
from super_router.failure import OFFERING_SCOPED, classify_failure, failure_error, parse_retry_after
from super_router.providers import AnthropicProvider, OpenAIProvider

pytestmark = pytest.mark.anyio

ROOT = Path(__file__).resolve().parents[3] / "conformance"
FAILURE = json.loads((ROOT / "failure.v1.json").read_text())
RETRY_AFTER = json.loads((ROOT / "retry_after.v1.json").read_text())
INVOCATION = {"input": {"messages": [{"role": "user", "content": "hi"}]}, "limits": {"max_output_tokens": 16}}


@pytest.mark.parametrize("case", FAILURE["cases"], ids=lambda c: c["name"])
def test_classification(case):
    cls = classify_failure(case["status"], case["message"])
    error = failure_error(cls, case["status"])
    want = case["expect"]
    assert (cls, error.definitive, error.retryable, error.code, cls in OFFERING_SCOPED) == (
        want["class"],
        want["definitive"],
        want["retryable"],
        want["code"],
        want["offering_scoped"],
    )


@pytest.mark.parametrize("case", RETRY_AFTER["cases"], ids=lambda c: repr(c["header"]))
def test_retry_after(case):
    assert parse_retry_after(case["header"]) == case["expect_ms"]


def _providers(handler):
    client = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    return {
        "openai": OpenAIProvider("k", "http://upstream", client),
        "anthropic": AnthropicProvider("k", "http://upstream", client),
    }


@pytest.mark.parametrize("name", ["openai", "anthropic"])
@pytest.mark.parametrize("case", FAILURE["provider_responses"], ids=lambda c: c["name"])
async def test_provider_responses(name, case):
    def handler(request: httpx.Request) -> httpx.Response:
        headers = {"retry-after": case["retry_after"]} if case["retry_after"] is not None else {}
        return httpx.Response(case["status"], content=case["body"].encode(), headers=headers)

    provider = _providers(handler)[name]
    with pytest.raises(ProviderError) as raised:
        _ = [e async for e in provider.invoke(INVOCATION, {"upstream_model": "m"})]
    error = raised.value
    want = case["expect"]
    assert (error.definitive, error.retryable, error.code, error.retry_after_ms) == (
        want["definitive"],
        want["retryable"],
        want["code"],
        want["retry_after_ms"],
    )
