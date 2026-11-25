# SPDX-License-Identifier: Apache-2.0
"""Wire contracts: strict parsing of untrusted JSON into a service or an invocation, the shared error types,
the provider port and the canonical JSON that feeds the idempotency digest.

Parsing mirrors packages/contracts of the TypeScript reference (zod ``.strict()``): unknown keys, ``null``,
out-of-range numbers, malformed ids or amounts all fail with ``invalid_request``. Parsed values are plain
dicts with the defaults filled, so they can be stored and serialised as they are. conformance/contracts.v1.json
is the shared test data. String limits count code points, as ``len(str)`` does.
"""

from __future__ import annotations

import json
import math
import re
from collections.abc import AsyncIterator, Mapping
from dataclasses import dataclass, field
from typing import Any, Protocol

from .pricing import MEDIA_KEYS, RouterError

# The largest output one model call may be asked for (wide enough for 128,000-token models) and the longest a request
# may run, in milliseconds. Same numbers in every backend (conformance/contracts.v1.json).
MAX_OUTPUT_TOKENS = 131072
MAX_TIMEOUT_MS = 600000

KIND_METER = {"image": "images", "video": "video_ms", "audio": "audio_ms", "task": "tasks"}
MAX_SAFE = 2**53 - 1

_ID = re.compile(r"[a-zA-Z0-9_.:/-]+")
_ATOMS = re.compile(r"0|[1-9][0-9]{0,29}")
_SCHEMA_NAME = re.compile(r"[a-zA-Z0-9_-]{1,64}")
_TAG_KEY = re.compile(r"[A-Za-z0-9_.:-]{1,64}")

Json = dict[str, Any]


def invalid_request() -> RouterError:
    return RouterError("invalid_request", 400)


class _Invalid(Exception):
    """Raised inside the parsers; converted to RouterError("invalid_request") at the public boundary."""


def _fail() -> _Invalid:
    return _Invalid()


def _has_null(value: Any) -> bool:
    if value is None:
        return True
    if isinstance(value, list):
        return any(_has_null(v) for v in value)
    if isinstance(value, dict):
        # A JSON Schema (a response format's `schema`, a tool's `input_schema`) may legitimately hold null.
        return any(
            _has_null(v) for k, v in value.items() if not (k in ("schema", "input_schema") and isinstance(v, dict))
        )
    return False


def _object(value: Any, required: tuple[str, ...], optional: tuple[str, ...] = ()) -> Json:
    """A dict with exactly the known keys, every required one present."""
    if not isinstance(value, dict) or not all(isinstance(k, str) for k in value):
        raise _fail()
    if not set(value) <= set(required) | set(optional) or not set(required) <= set(value):
        raise _fail()
    return dict(value)


def _int(value: Any, low: int = -MAX_SAFE, high: int = MAX_SAFE) -> int:
    if isinstance(value, bool):
        raise _fail()
    if isinstance(value, float):
        if not value.is_integer():
            raise _fail()
        value = int(value)
    if not isinstance(value, int) or not low <= value <= high:
        raise _fail()
    return value


def _number(value: Any) -> float | int:
    if isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value):
        raise _fail()
    return value


def _string(value: Any, low: int = 0, high: int = 10**9) -> str:
    if not isinstance(value, str) or not low <= len(value) <= high:
        raise _fail()
    return value


def _id(value: Any) -> str:
    text = _string(value, 1, 120)
    if _ID.fullmatch(text) is None:
        raise _fail()
    return text


def _atoms(value: Any) -> str:
    if not isinstance(value, str) or _ATOMS.fullmatch(value) is None:
        raise _fail()
    return value


def _bool(value: Any) -> bool:
    if not isinstance(value, bool):
        raise _fail()
    return value


def _list(value: Any, low: int = 0, high: int = 10**9) -> list[Any]:
    if not isinstance(value, list) or not low <= len(value) <= high:
        raise _fail()
    return value


def _task_kind(value: Any) -> str:
    if value not in KIND_METER:
        raise _fail()
    return str(value)


def _media_meters(value: Any) -> Json:
    obj = _object(value, (), MEDIA_KEYS)
    return {k: _int(v, 0) for k, v in obj.items()}


def _price(value: Any) -> Json:
    obj = _object(value, ("asset", "input", "cached", "cache_write", "output"), ("per", "media"))
    out: Json = {"asset": _id(obj["asset"])}
    for key in ("input", "cached", "cache_write", "output"):
        out[key] = _atoms(obj[key])
    out["per"] = _int(obj.get("per", 1), 1, 1_000_000_000)
    if "media" in obj:
        media = _object(obj["media"], (), MEDIA_KEYS)
        out["media"] = {}
        for key, rate in media.items():
            r = _object(rate, ("atoms", "per"))
            out["media"][key] = {"atoms": _atoms(r["atoms"]), "per": _int(r["per"], 1, 1_000_000_000)}
    return out


_OFFERING_REQUIRED = (
    "id",
    "provider",
    "model",
    "upstream_model",
    "region",
    "enabled",
    "source",
    "max_input_tokens",
    "max_output_tokens",
    "price",
)
_OFFERING_OPTIONAL = ("task_kind", "max_usage", "labels", "latency_ms", "reliability", "quality")


def _offering(value: Any) -> Json:
    obj = _object(value, _OFFERING_REQUIRED, _OFFERING_OPTIONAL)
    out: Json = {
        "id": _id(obj["id"]),
        "provider": _id(obj["provider"]),
        "model": _id(obj["model"]),
        "upstream_model": _id(obj["upstream_model"]),
        "region": _id(obj["region"]),
        "labels": [_id(x) for x in _list(obj.get("labels", []))],
        "enabled": _bool(obj["enabled"]),
        "source": obj["source"],
        "max_input_tokens": _int(obj["max_input_tokens"], 1, 1_000_000),
        "max_output_tokens": _int(obj["max_output_tokens"], 1, MAX_OUTPUT_TOKENS),
        "price": _price(obj["price"]),
    }
    if out["source"] not in ("platform", "byok"):
        raise _fail()
    if "task_kind" in obj:
        out["task_kind"] = _task_kind(obj["task_kind"])
    if "max_usage" in obj:
        out["max_usage"] = _media_meters(obj["max_usage"])
    if "latency_ms" in obj:
        latency = _number(obj["latency_ms"])
        if latency < 0:
            raise _fail()
        out["latency_ms"] = latency
    for key in ("reliability", "quality"):
        if key in obj:
            score = _number(obj[key])
            if not 0 <= score <= 1:
                raise _fail()
            out[key] = score
    return out


_SERVICE_REQUIRED = (
    "id",
    "version",
    "mode",
    "offerings",
    "regions",
    "price",
    "max_attempts",
    "max_provider_cost",
    "max_output_tokens",
    "weights",
    "cost_scale",
    "latency_scale_ms",
)
_SERVICE_OPTIONAL = (
    "task_kind",
    "model",
    "tier",
    "required_labels",
    "platform_price",
    "requires_subscription",
    "unknown_score",
)


def _service(value: Any) -> Json:
    obj = _object(value, _SERVICE_REQUIRED, _SERVICE_OPTIONAL)
    weights_in = _object(obj["weights"], ("cost", "latency", "reliability", "quality"))
    weights = {k: _number(v) for k, v in weights_in.items()}
    out: Json = {
        "id": _id(obj["id"]),
        "version": _int(obj["version"], 1),
        "mode": obj["mode"],
        "offerings": [_offering(o) for o in _list(obj["offerings"], 1)],
        "regions": [_id(r) for r in _list(obj["regions"], 1)],
        "required_labels": [_id(x) for x in _list(obj.get("required_labels", []))],
        "price": _price(obj["price"]),
        "max_attempts": _int(obj["max_attempts"], 1, 5),
        "max_provider_cost": _atoms(obj["max_provider_cost"]),
        "max_output_tokens": _int(obj["max_output_tokens"], 1, MAX_OUTPUT_TOKENS),
        "weights": weights,
        "cost_scale": _atoms(obj["cost_scale"]),
        "latency_scale_ms": _number(obj["latency_scale_ms"]),
        "unknown_score": _number(obj.get("unknown_score", 0)),
    }
    if out["mode"] not in ("specific", "pool", "tier"):
        raise _fail()
    if "task_kind" in obj:
        out["task_kind"] = _task_kind(obj["task_kind"])
    for key in ("model", "tier"):
        if key in obj:
            out[key] = _id(obj[key])
    if "platform_price" in obj:
        out["platform_price"] = _price(obj["platform_price"])
    if "requires_subscription" in obj:
        out["requires_subscription"] = _bool(obj["requires_subscription"])
    if any(w < 0 for w in weights.values()) or abs(sum(weights.values()) - 1) > 1e-9:
        raise _fail()
    if out["mode"] == "specific" and "model" not in out or out["mode"] == "tier" and "tier" not in out:
        raise _fail()
    ids = [o["id"] for o in out["offerings"]]
    if len(set(ids)) != len(ids) or len({o["price"]["asset"] for o in out["offerings"]}) != 1:
        raise _fail()
    if int(out["cost_scale"]) <= 0 or out["latency_scale_ms"] <= 0 or not 0 <= out["unknown_score"] <= 1:
        raise _fail()
    return out


def _sampling(value: Any) -> Json:
    obj = _object(value, (), ("temperature", "top_p", "stop", "seed"))
    if not obj:
        raise _fail()
    out: Json = {}
    for key, low, high in (("temperature", 0, 2), ("top_p", 0, 1)):
        if key in obj:
            number = _number(obj[key])
            if not low <= number <= high or (key == "top_p" and number == 0):
                raise _fail()
            # An integral float prints as an integer in JSON.stringify, so it is stored as one.
            out[key] = int(number) if isinstance(number, float) and number.is_integer() else number
    if "stop" in obj:
        out["stop"] = [_string(x, 1, 64) for x in _list(obj["stop"], 1, 4)]
    if "seed" in obj:
        out["seed"] = _int(obj["seed"])
    return out


def _tags(value: Any) -> Json:
    if not isinstance(value, dict) or not 1 <= len(value) <= 16:
        raise _fail()
    out: Json = {}
    for key, text in value.items():
        if not isinstance(key, str) or _TAG_KEY.fullmatch(key) is None:
            raise _fail()
        out[key] = _string(text, 1, 256)
        if any(ord(c) < 0x20 or ord(c) == 0x7F for c in text):
            raise _fail()
    return out


def _response_format(value: Any) -> Json:
    obj = _object(value, ("type", "json_schema"))
    if obj["type"] != "json_schema":
        raise _fail()
    js = _object(obj["json_schema"], ("name", "schema"), ("description", "strict"))
    name = _string(js["name"])
    if _SCHEMA_NAME.fullmatch(name) is None or not isinstance(js["schema"], dict):
        raise _fail()
    out: Json = {"name": name, "schema": js["schema"]}
    if "description" in js:
        out["description"] = _string(js["description"], 0, 1024)
    if "strict" in js:
        # A schema is always enforced; `false` would ask for a best effort the router cannot promise.
        if js["strict"] is not True:
            raise _fail()
        out["strict"] = True
    return {"type": "json_schema", "json_schema": out}


def _invocation(value: Any) -> Json:
    obj = _object(
        value,
        ("service", "customer_id", "input", "limits"),
        ("service_version", "payer_mode", "stream", "response_format", "sampling", "reasoning_effort", "tags"),
    )
    inp = _object(obj["input"], (), ("messages", "task"))
    messages = []
    for m in _list(inp.get("messages", []), 0, 100):
        message = _object(m, ("role", "content"))
        if message["role"] not in ("system", "user", "assistant"):
            raise _fail()
        messages.append({"role": message["role"], "content": _string(message["content"], 1, 32000)})
    parsed_input: Json = {"messages": messages}
    if "task" in inp:
        task = _object(inp["task"], ("kind", "prompt"))
        parsed_input["task"] = {"kind": _task_kind(task["kind"]), "prompt": _string(task["prompt"], 1, 32000)}
    limits = _object(obj["limits"], (), ("max_output_tokens", "max_usage", "timeout_ms", "max_charge"))
    parsed_limits: Json = {
        "max_output_tokens": _int(limits.get("max_output_tokens", 1), 1, MAX_OUTPUT_TOKENS),
        "timeout_ms": _int(limits.get("timeout_ms", 30000), 100, MAX_TIMEOUT_MS),
    }
    if "max_usage" in limits:
        parsed_limits["max_usage"] = _media_meters(limits["max_usage"])
    if "max_charge" in limits:
        charge = _object(limits["max_charge"], ("asset", "atoms", "scale"))
        if _int(charge["scale"]) != 9:
            raise _fail()
        parsed_limits["max_charge"] = {"asset": _id(charge["asset"]), "atoms": _atoms(charge["atoms"]), "scale": 9}
    out: Json = {
        "service": _id(obj["service"]),
        "customer_id": _id(obj["customer_id"]),
        "payer_mode": obj.get("payer_mode", "developer"),
        "input": parsed_input,
        "limits": parsed_limits,
        "stream": _bool(obj.get("stream", False)),
    }
    if out["payer_mode"] != "developer":
        raise _fail()
    if "service_version" in obj:
        out["service_version"] = _int(obj["service_version"], 1)
    if "response_format" in obj:
        if "task" in parsed_input:
            raise _fail()
        out["response_format"] = _response_format(obj["response_format"])
    if "sampling" in obj:
        out["sampling"] = _sampling(obj["sampling"])
    if "reasoning_effort" in obj:
        if obj["reasoning_effort"] not in ("low", "medium", "high"):
            raise _fail()
        out["reasoning_effort"] = obj["reasoning_effort"]
    if "tags" in obj:
        out["tags"] = _tags(obj["tags"])
    return out


def invocation_needs(inv: Mapping[str, Any]) -> frozenset[str]:
    """What a request needs from a provider beyond plain text (the names a provider lists in ``capabilities``):
    ``structured_output`` for a ``response_format``, one per sampling control set, and ``reasoning_effort``. (Tools and
    images are refused at parsing: this backend has no tool calling yet.)"""
    needs = {key for key in ("temperature", "top_p", "stop", "seed") if key in inv.get("sampling", {})}
    if "response_format" in inv:
        needs.add("structured_output")
    if "reasoning_effort" in inv:
        needs.add("reasoning_effort")
    return frozenset(needs)


def parse_service(value: Any) -> Json:
    """Validate a service definition; raise RouterError("invalid_request") otherwise."""
    try:
        if _has_null(value):
            raise _fail()
        return _service(value)
    except _Invalid:
        raise invalid_request() from None


def parse_invocation(value: Any) -> Json:
    """Validate an invocation and fill its defaults; raise RouterError("invalid_request") otherwise."""
    try:
        if _has_null(value):
            raise _fail()
        return _invocation(value)
    except _Invalid:
        raise invalid_request() from None


def valid_meters(value: Any) -> Json | None:
    """A provider usage record: four token meters plus optional media meters, all non-negative safe integers."""
    try:
        obj = _object(value, ("input", "cached", "cache_write", "output"), MEDIA_KEYS)
        return {k: _int(v, 0) for k, v in obj.items()}
    except _Invalid:
        return None


def canonical(value: Any) -> str:
    """JSON with keys sorted and no whitespace: the input of the idempotency digest, identical in every language."""
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False)


ROLE_SCOPES: dict[str, tuple[str, ...]] = {
    "owner": (
        "invoke",
        "identity:write",
        "service:write",
        "routing:write",
        "billing:write",
        "billing:read",
        "operations:read",
    ),
    "service_admin": ("invoke", "service:write", "routing:write", "operations:read"),
    "billing_admin": ("billing:write", "billing:read", "operations:read"),
    "viewer": ("billing:read", "operations:read"),
    "app": ("invoke", "credits:read"),
}


@dataclass(frozen=True)
class Principal:
    """An authenticated caller. Hosts must only construct one from verified credentials."""

    workspace: str
    environment: str
    customer: str
    scopes: tuple[str, ...] = ()
    auth_key_id: str | None = None
    member_id: str | None = None
    service_ids: tuple[str, ...] | None = None

    def has_scope(self, scope: str) -> bool:
        return scope in self.scopes

    def may_use(self, service: str) -> bool:
        return self.service_ids is None or service in self.service_ids


class ProviderError(Exception):
    """A failed upstream call. ``definitive`` means the provider certainly did no work, so the reservation can be
    released; ``retryable`` means another offering may be tried."""

    def __init__(
        self, definitive: bool, retryable: bool, code: str = "upstream_failed", retry_after_ms: int | None = None
    ) -> None:
        super().__init__(code)
        self.definitive = definitive
        self.retryable = retryable
        self.code = code
        # How long the provider asked callers to wait (its Retry-After), in milliseconds. Only a hint.
        self.retry_after_ms = retry_after_ms


@dataclass
class Delta:
    text: str


@dataclass
class Usage:
    meters: Json
    upstream_id: str | None = None


@dataclass
class Done:
    completion: str = "succeeded"  # or "partial"


ProviderEvent = Delta | Usage | Done


class Provider(Protocol):
    """Runs one invocation against an upstream model, yielding events as they arrive. Raise ProviderError on failure.
    The router cancels the iteration (asyncio cancellation) on timeout, cancel request or client disconnect."""

    def invoke(self, invocation: Mapping[str, Any], offering: Mapping[str, Any]) -> AsyncIterator[ProviderEvent]: ...

    # Optional attribute ``capabilities: frozenset[str]``: the request features beyond plain text the provider carries
    # (see :func:`invocation_needs`). A provider without it carries none, so such a request is refused before anything
    # is reserved rather than answered without them.


@dataclass
class StreamEvent:
    type: str
    request_id: str
    sequence: int
    data: Json = field(default_factory=dict)

    def as_dict(self) -> Json:
        return {"type": self.type, "request_id": self.request_id, "sequence": self.sequence, **self.data}
