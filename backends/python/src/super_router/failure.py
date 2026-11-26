# SPDX-License-Identifier: Apache-2.0
"""Why an upstream call was refused, and what that means for the next offering.

A failed call says two different things. A failure of the **request** (malformed, too large) would be refused by every
offering, so the call stops. A failure of the **offering** (its key was rejected, its model is gone, its account is
empty, its region is blocked, it is rate limited) is certain to have done no work, and another offering may serve the
request. Server errors and unknown statuses are neither: the provider may have started work, so the reservation stays
held and the call is never retried for free. ``conformance/failure.v1.json`` and ``retry_after.v1.json`` are the shared
data every backend runs (the health book that uses the classes is Rust and Go only).
"""

from __future__ import annotations

import json
import re
from typing import Any

from .contracts import ProviderError

RATE_LIMITED = "rate_limited"
QUOTA_EXHAUSTED = "quota_exhausted"
CREDENTIALS_REJECTED = "credentials_rejected"
MODEL_UNAVAILABLE = "model_unavailable"
REGION_BLOCKED = "region_blocked"
REQUEST_REJECTED = "request_rejected"
UNKNOWN = "unknown"

OFFERING_SCOPED = frozenset({RATE_LIMITED, QUOTA_EXHAUSTED, CREDENTIALS_REJECTED, MODEL_UNAVAILABLE, REGION_BLOCKED})

# Phrases are written with spaces: the message is lower-cased and its underscores and hyphens read as spaces first.
_CREDENTIAL = (
    "api key not valid",
    "api key expired",
    "api key invalid",
    "invalid api key",
    "incorrect api key",
    "invalid x api key",
)
_QUOTA = (
    "insufficient quota",
    "exceeded your current quota",
    "credit balance is too low",
    "credits are depleted",
    "spend limit",
    "spending cap",
    "requires billing to be enabled",
    "billing account",
    "per day",
)
_REGION = (
    "user location is not supported",
    "not available in your country",
    "not available in your region",
    "not supported in your region",
)
_MODEL = ("model not found", "is not found for api version", "does not exist or you do not have access")

# The longest wait a Retry-After can ask for, in milliseconds.
RETRY_AFTER_MAX_MS = 86_400_000
_DELTA_SECONDS = re.compile(r"([0-9]+)(?:\.([0-9]+))?")
# How much of a client error's body is read for its message.
BODY_LIMIT = 16_384


def classify_failure(status: int, message: str = "") -> str:
    """The failure class of a refusal, from its HTTP status and the error's own message (may be empty)."""
    text = message.lower().replace("_", " ").replace("-", " ")

    def said(phrases: tuple[str, ...]) -> bool:
        return any(p in text for p in phrases)

    if said(_CREDENTIAL):
        return CREDENTIALS_REJECTED
    if said(_REGION):
        return REGION_BLOCKED
    if status == 401:
        return CREDENTIALS_REJECTED
    if status == 402:
        return QUOTA_EXHAUSTED
    if status in (400, 403, 429) and said(_QUOTA):
        return QUOTA_EXHAUSTED
    if status == 403:
        return CREDENTIALS_REJECTED
    if status == 404:
        return MODEL_UNAVAILABLE
    if status == 429:
        return RATE_LIMITED
    if status == 400 and said(_MODEL):
        return MODEL_UNAVAILABLE
    # Any other client error is the request's: 408 (a timeout the provider may have acted on) is not.
    if 400 <= status < 500 and status != 408:
        return REQUEST_REJECTED
    return UNKNOWN


def failure_error(failure_class: str, status: int, retry_after_ms: int | None = None) -> ProviderError:
    """The provider error for a class: offering-scoped is definitive and retryable, a rejected request is definitive
    and not, anything else keeps the hold."""
    if failure_class in OFFERING_SCOPED:
        error = ProviderError(True, True, failure_class)
    else:
        error = ProviderError(failure_class == REQUEST_REJECTED, False, f"upstream_http_{status}")
    error.retry_after_ms = retry_after_ms
    return error


def parse_retry_after(value: str | None) -> int | None:
    """The value of a ``Retry-After`` header as milliseconds. Only the delta-seconds form is read: a non-negative number
    of seconds, whole or with a fraction, surrounding spaces and tabs ignored, milliseconds rounded down, capped at a
    day. The HTTP-date form is not read (None)."""
    match = _DELTA_SECONDS.fullmatch((value or "").strip(" \t"))
    if match is None:
        return None
    whole, fraction = match.group(1), match.group(2) or ""
    seconds = RETRY_AFTER_MAX_MS // 1000 if len(whole) > 9 else int(whole)
    millis = int(fraction[:3].ljust(3, "0"))
    return min(seconds * 1000 + millis, RETRY_AFTER_MAX_MS)


def refusal(status: int, retry_after: str | None, body: bytes) -> ProviderError:
    """The provider error for an HTTP refusal: the status, the first 16 KiB of a client error's body (only its
    ``error.message`` is read, nothing of it is passed on) and the ``Retry-After`` header. A server error's body is not
    read."""
    message = ""
    if 400 <= status < 500:
        try:
            parsed: Any = json.loads(body[:BODY_LIMIT])
            text = parsed["error"]["message"]
            message = text if isinstance(text, str) else ""
        except (ValueError, KeyError, TypeError):
            message = ""
    return failure_error(classify_failure(status, message), status, parse_retry_after(retry_after))
