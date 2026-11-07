# SPDX-License-Identifier: Apache-2.0
"""Offering selection, ported from `route` in the TypeScript reference (packages/core).

Hard limits first, then weighted scoring, ordered by score and then by id. It reproduces every
case in conformance/routing.v1.json, including the exact IEEE-754 scores. Offering ids must sort
the same byte-wise as in TypeScript (use [a-z0-9]).
"""

from __future__ import annotations

from collections.abc import Mapping
from dataclasses import dataclass
from typing import Any

from .pricing import bound, charge

MEDIA_KEYS = ("images", "video_ms", "audio_ms", "tasks")
_KIND_METER = {"image": "images", "video": "video_ms", "audio": "audio_ms", "task": "tasks"}


@dataclass
class Candidate:
    offering: Mapping[str, Any]
    cost: int
    score: float


def _invocation_bound(invocation: Mapping[str, Any], max_input: int, price: Mapping[str, Any]) -> int:
    limits = invocation["limits"]
    if not invocation["input"].get("task"):
        return bound(max_input, limits["max_output_tokens"], price)
    usage = {"input": 0, "cached": 0, "cache_write": 0, "output": 0}
    usage.update(limits.get("max_usage") or {})
    return charge(usage, price)


def route(service: Mapping[str, Any], invocation: Mapping[str, Any]) -> tuple[list[Candidate], list[dict[str, str]]]:
    """Return (candidates best first, excluded with reasons). May raise RouterError from pricing."""
    task = invocation["input"].get("task")
    task_kind = task["kind"] if task else None
    limits = invocation["limits"]
    max_provider_cost = int(service["max_provider_cost"])
    cost_scale = int(service["cost_scale"])
    weights = service["weights"]
    unknown = service["unknown_score"]
    candidates: list[Candidate] = []
    excluded: list[dict[str, str]] = []
    for o in service["offerings"]:
        reason = ""
        if o.get("task_kind") != task_kind or service.get("task_kind") != task_kind:
            reason = "task_kind_mismatch"
        elif not o["enabled"]:
            reason = "disabled"
        elif service["mode"] == "specific" and o["model"] != service.get("model"):
            reason = "model_mismatch"
        elif o["region"] not in service["regions"]:
            reason = "region_mismatch"
        elif any(x not in o["labels"] for x in service["required_labels"]):
            reason = "label_mismatch"
        elif limits["max_output_tokens"] > o["max_output_tokens"]:
            reason = "output_limit"
        if not reason and task:
            key = _KIND_METER[task["kind"]]
            units = (limits.get("max_usage") or {}).get(key)
            offered = (o.get("max_usage") or {}).get(key, 0)
            media = o["price"].get("media") or {}
            if not units or units > offered or not media.get(key):
                reason = "meter_limit"
        cost = 0
        if not reason:
            cost = _invocation_bound(invocation, o["max_input_tokens"], o["price"])
            if cost > max_provider_cost:
                reason = "budget_exceeded"
        if reason:
            excluded.append({"id": o["id"], "reason": reason})
            continue
        # Number(bigint) rounds to nearest even, exactly like float(int).
        ratio = float((cost * 1_000_000) // cost_scale) / 1_000_000
        latency_ms = o.get("latency_ms")
        latency = unknown if latency_ms is None else 1 - min(1, latency_ms / service["latency_scale_ms"])
        reliability = o.get("reliability")
        quality = o.get("quality")
        score = (
            weights["cost"] * (1 - min(1, ratio))
            + weights["latency"] * latency
            + weights["reliability"] * (unknown if reliability is None else reliability)
            + weights["quality"] * (unknown if quality is None else quality)
        )
        candidates.append(Candidate(o, cost, score))
    candidates.sort(key=lambda c: (-c.score, c.offering["id"]))
    return candidates, excluded
