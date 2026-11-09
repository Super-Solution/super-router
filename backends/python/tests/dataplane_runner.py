# SPDX-License-Identifier: Apache-2.0
"""Runs conformance/dataplane.v1.json against any DataStore. The same file drives the TypeScript and Go ports."""

from __future__ import annotations

import copy
import json
import uuid
from collections.abc import Awaitable, Callable
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from super_router import RouterError
from super_router.contracts import Principal, parse_service
from super_router.router import DataStore, MockProvider, Router

DATA = json.loads((Path(__file__).resolve().parents[3] / "conformance" / "dataplane.v1.json").read_text())
DIGEST_KEY = "conformance-digest-key-0123456789abcdef"


@dataclass
class Harness:
    store: DataStore
    clock: Callable[[], int]  # epoch ms: virtual for in-memory stores, the real clock for SQL stores
    advance: Callable[[int], Awaitable[None]]
    buckets: Callable[[Principal], Awaitable[list[dict[str, str]]]]


def merge(base: dict[str, Any], patch: dict[str, Any]) -> dict[str, Any]:
    out = copy.deepcopy(base)
    for key, value in patch.items():
        if isinstance(value, dict) and isinstance(base.get(key), dict):
            out[key] = merge(base[key], value)
        else:
            out[key] = copy.deepcopy(value)
    return out


def matches(actual: Any, expected: Any, refs: dict[str, str], path: str) -> None:
    if isinstance(expected, str):
        if expected.startswith("$"):
            assert actual == refs[expected[1:]], path
        elif expected == ">0":
            assert isinstance(actual, str) and int(actual) > 0, f"{path}: {actual!r} is not > 0"
        else:
            assert actual == expected, f"{path}: {actual!r} != {expected!r}"
    elif isinstance(expected, list):
        assert isinstance(actual, list) and len(actual) == len(expected), f"{path}: {actual!r} != {expected!r}"
        for i, item in enumerate(expected):
            matches(actual[i], item, refs, f"{path}[{i}]")
    elif isinstance(expected, dict):
        assert isinstance(actual, dict), f"{path}: expected an object, got {actual!r}"
        for key, item in expected.items():
            matches(actual.get(key), item, refs, f"{path}.{key}")
    else:
        assert type(actual) is type(expected) and actual == expected, f"{path}: {actual!r} != {expected!r}"


def scenarios() -> list[dict[str, Any]]:
    return list(DATA["scenarios"])  # type: ignore[no-any-return]


async def run_scenario(scenario: dict[str, Any], h: Harness) -> None:
    refs: dict[str, str] = {}
    behaviors = scenario.get("providers", DATA["providers"])
    providers = {name: MockProvider(behavior) for name, behavior in behaviors.items()}
    router = Router(h.store, providers, DIGEST_KEY, h.clock)
    workspace = "ws-" + str(uuid.uuid4())
    principals = {
        name: Principal(
            workspace,
            "test",
            p["customer"],
            tuple(p["scopes"]),
            service_ids=tuple(p["service_ids"]) if "service_ids" in p else None,
        )
        for name, p in DATA["principals"].items()
    }

    def who(step: dict[str, Any]) -> Principal:
        return principals[step.get("principal", "owner")]

    def service(step: dict[str, Any]) -> dict[str, Any]:
        return merge(DATA["services"][step["service"]], step.get("patch", {}))

    for index, step in enumerate(scenario["steps"]):
        op = step["op"]
        label = f"{scenario['name']} #{index} {op}"
        result: Any = None
        failure: RouterError | None = None
        try:
            if op == "publish":
                await h.store.publish(who(step), service(step), step.get("mode", "self-host"))
            elif op == "grant":
                expires = step.get("expires_in_ms")
                result = await h.store.grant(
                    who(step),
                    step["kind"],
                    step["asset"],
                    step["amount"],
                    step["source"],
                    None if expires is None else h.clock() + expires,
                )
            elif op == "infer":
                invocation = merge(DATA["invocation"], step.get("input", {}))
                result = await router.infer(who(step), invocation, step.get("key", f"key-{index}"))
                if "as" in step:
                    refs[step["as"]] = result["id"]
            elif op == "balance":
                result = [row.to_json() for row in await h.store.balance(who(step))]
            elif op == "buckets":
                result = await h.buckets(who(step))
            elif op == "get":
                r = await h.store.get(who(step), refs.get(step["request"], step["request"]))
                result = {
                    "execution": r.execution,
                    "billing": r.billing,
                    "cancel_requested": r.cancel_requested,
                    "output_started": r.output_started,
                    "provider_spent": r.provider_spent,
                }
            elif op == "cancel":
                await h.store.cancel(who(step), refs.get(step["request"], step["request"]))
            elif op == "advance":
                await h.advance(step["ms"])
            elif op == "disable":
                await h.store.disable(who(step), step["offering"])
            elif op == "recover":
                result = await h.store.recover()
            elif op == "provider_calls":
                result = providers[step["provider"]].calls
            elif op == "reserve":
                s = parse_service(service(step))
                p = who(step)
                legs = await h.store.accounts(p, s)
                for leg in legs:
                    leg.reserved = step["reserved"].get(leg.kind, "0")
                record, fresh = await h.store.reserve(
                    p, step["key"], step.get("digest", "d"), s, legs, h.clock() + step["timeout_ms"]
                )
                refs[step["as"]] = record.id
                result = {"fresh": fresh, "execution": record.execution, "billing": record.billing}
            elif op == "begin":
                s = parse_service(service(step))
                offering = next(o for o in s["offerings"] if o["id"] == step["offering"])
                attempt = await h.store.begin(
                    who(step), refs.get(step["request"], step["request"]), offering, step["bound"]
                )
                if "as" in step:
                    refs[step["as"]] = attempt
            elif op == "attempt":
                await h.store.attempt_result(
                    who(step),
                    refs.get(step["request"], step["request"]),
                    refs.get(step["attempt"], step["attempt"]),
                    step["status"],
                    step.get("usage"),
                    step.get("cost"),
                )
            elif op == "output":
                await h.store.output(who(step), refs.get(step["request"], step["request"]))
            elif op == "finish":
                await h.store.finish(
                    who(step),
                    refs.get(step["request"], step["request"]),
                    step["execution"],
                    step.get("usage"),
                    step.get("pending", False),
                )
            else:
                raise AssertionError(f"unknown op {op}")
        except RouterError as e:
            failure = e
        if "error" in step:
            assert failure is not None, f"{label}: expected error {step['error']['code']}"
            assert failure.code == step["error"]["code"], label
            if "status" in step["error"]:
                assert failure.status == step["error"]["status"], label
            continue
        assert failure is None, f"{label}: unexpected {failure.code if failure else ''}"
        if "expect" in step:
            matches(result, step["expect"], refs, label)
