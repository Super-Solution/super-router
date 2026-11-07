# SPDX-License-Identifier: Apache-2.0
"""Super Router pricing core in Python.

Mirrors packages/core (charge, bound) and packages/core/src/credit-rates.ts (compile_credit_price).
All money is integer atoms held as Python ints (arbitrary precision), so results match the
TypeScript BigInt reference exactly. conformance/pricing.v1.json is the shared test data.
"""

from __future__ import annotations

import re
from collections.abc import Mapping
from math import gcd
from typing import Any

CREDIT_ATOMS = 1_000_000_000
MEDIA_KEYS = ("images", "video_ms", "audio_ms", "tasks")
TOKEN_KEYS = ("input", "cached", "cache_write", "output")

_INTEGER = re.compile(r"[1-9][0-9]{0,29}")
_ATOMS = re.compile(r"0|[1-9][0-9]{0,29}")
_ID = re.compile(r"[a-zA-Z0-9_.:/-]+")


class RouterError(Exception):
    def __init__(self, code: str, status: int = 400, retryable: bool = False) -> None:
        super().__init__(code)
        self.code = code
        self.status = status
        self.retryable = retryable


def charge(usage: Mapping[str, int], price: Mapping[str, Any]) -> int:
    """Customer charge in atoms for one usage record, rounded up per meter group."""
    numerator = sum(int(usage[k]) * int(price[k]) for k in TOKEN_KEYS)
    per = int(price["per"])
    total = (numerator + per - 1) // per
    media = price.get("media") or {}
    for key in MEDIA_KEYS:
        units = usage.get(key) or 0
        rate = media.get(key)
        if units and not rate:
            raise RouterError("unpriced_meter", 422)
        if rate:
            total += (int(units) * int(rate["atoms"]) + int(rate["per"]) - 1) // int(rate["per"])
    return total


def bound(input_tokens: int, output_tokens: int, price: Mapping[str, Any]) -> int:
    """Upper bound reserved before a call: the dearest input rate times input, plus output."""
    rate = max(int(price["input"]), int(price["cached"]), int(price["cache_write"]))
    per = int(price["per"])
    return (input_tokens * rate + output_tokens * int(price["output"]) + per - 1) // per


def _invalid() -> RouterError:
    return RouterError("invalid_credit_rates")


def _ratio(value: Any) -> tuple[int, int]:
    """Validate {units, credits?} strictly; return (units, credits)."""
    if not isinstance(value, dict) or not set(value) <= {"units", "credits"} or "units" not in value:
        raise _invalid()
    units, credits = value["units"], value.get("credits", "1")
    for item in (units, credits):
        if not isinstance(item, str) or _INTEGER.fullmatch(item) is None:
            raise _invalid()
    return int(units), int(credits)


def _rate(units: int, credits: int) -> tuple[int, int]:
    numerator, denominator = CREDIT_ATOMS * credits, units
    g = gcd(numerator, denominator)
    return numerator // g, denominator // g


def compile_credit_price(raw: Any) -> dict[str, Any]:
    """Compile exact rational units-per-credit into the wire Price."""
    if not isinstance(raw, dict) or not set(raw) <= {"asset", "tokens", "media"}:
        raise _invalid()
    asset = raw.get("asset")
    if not isinstance(asset, str) or not 1 <= len(asset) <= 120 or _ID.fullmatch(asset) is None:
        raise _invalid()
    tokens = raw.get("tokens")
    media = raw.get("media")
    parsed_tokens: dict[str, tuple[int, int]] | None = None
    if "tokens" in raw:
        if not isinstance(tokens, dict) or set(tokens) != set(TOKEN_KEYS):
            raise _invalid()
        parsed_tokens = {k: _ratio(tokens[k]) for k in TOKEN_KEYS}
    parsed_media: dict[str, tuple[int, int]] = {}
    if "media" in raw:
        if not isinstance(media, dict) or not set(media) <= set(MEDIA_KEYS):
            raise _invalid()
        parsed_media = {k: _ratio(v) for k, v in media.items()}
    if parsed_tokens is None and not parsed_media:
        raise _invalid()

    rates = [_rate(*parsed_tokens[k]) if parsed_tokens is not None else (0, 1) for k in TOKEN_KEYS]
    per = 1
    for _, denominator in rates:
        per = per // gcd(per, denominator) * denominator
        if per > 1_000_000_000:
            raise RouterError("unsupported_rate_precision", 422)
    price: dict[str, Any] = {"asset": asset, "per": per}
    for key, (numerator, denominator) in zip(TOKEN_KEYS, rates, strict=True):
        price[key] = str(numerator * (per // denominator))
    media_out: dict[str, dict[str, Any]] = {}
    for key in MEDIA_KEYS:
        if key not in parsed_media:
            continue
        numerator, denominator = _rate(*parsed_media[key])
        if denominator > 1_000_000_000:
            raise RouterError("unsupported_rate_precision", 422)
        media_out[key] = {"atoms": str(numerator), "per": denominator}
    if media_out:
        price["media"] = media_out
    # The wire Price caps atoms at 30 digits.
    atoms = [price[k] for k in TOKEN_KEYS] + [m["atoms"] for m in media_out.values()]
    if any(_ATOMS.fullmatch(a) is None for a in atoms):
        raise RouterError("unsupported_rate_precision", 422)
    return price
