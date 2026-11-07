# SPDX-License-Identifier: Apache-2.0
"""The Super Router data plane: admit an invocation idempotently, pick offerings, reserve the customer's funds,
run the provider and settle or release the reservation.

It mirrors ``Router.infer`` of the TypeScript reference. Storage is the :class:`Store` port, providers are the
:class:`~super_router.contracts.Provider` port, and nothing here touches HTTP, so any framework can host it.
An uncertain provider outcome is never refunded: the reservation stays pending for reconciliation.
"""

from __future__ import annotations

import asyncio
import contextlib
import hashlib
import hmac
import time
import uuid
from collections.abc import AsyncIterator, Awaitable, Callable, Mapping
from dataclasses import dataclass, field
from typing import Any, Protocol

from .contracts import (
    KIND_METER,
    Delta,
    Done,
    Json,
    Principal,
    Provider,
    ProviderError,
    ProviderEvent,
    StreamEvent,
    Usage,
    canonical,
    parse_invocation,
    valid_meters,
)
from .pricing import MEDIA_KEYS, RouterError, bound, charge
from .routing import Candidate, route

Clock = Callable[[], int]
"""Epoch milliseconds. Stores and the router accept one so tests control time."""


def wall_clock() -> int:
    return int(time.time() * 1000)


@dataclass
class Leg:
    """One account a request charges: the customer's sale account and, optionally, the platform's."""

    kind: str  # "customer_sale" or "platform_sale"
    account: str
    price: Json
    reserved: str = "0"  # atoms held before the call

    def to_json(self) -> Json:
        return {"kind": self.kind, "account": self.account, "price": self.price, "reserved": self.reserved}

    @staticmethod
    def from_json(data: Mapping[str, Any]) -> Leg:
        return Leg(data["kind"], data["account"], dict(data["price"]), data["reserved"])


@dataclass
class RequestRecord:
    """What a store knows about one request. Treat it as read-only."""

    id: str
    workspace: str
    environment: str
    customer: str
    service: Json
    execution: str  # accepted, running, succeeded, partial, unknown, failed, cancelled
    billing: str  # reserved, pending, settled, released
    cancel_requested: bool
    output_started: bool
    deadline: int  # epoch ms
    legs: list[Leg]
    provider_spent: str


@dataclass
class BalanceRow:
    id: str
    kind: str
    asset: str
    balance: str
    held: str
    available: str

    def to_json(self) -> Json:
        return dict(self.__dict__)


class Store(Protocol):
    """The storage port of the router. Every method must be atomic and safe for concurrent use.

    Implement it (and :class:`DataStore`) to plug in another database, then run
    conformance/dataplane.v1.json against it (see tests/test_dataplane.py)."""

    async def replay(self, p: Principal, key: str, digest: str) -> RequestRecord | None:
        """The request admitted under ``key``, None if none, or idempotency_conflict (409)."""
        ...

    async def get_service(self, p: Principal, id: str, version: int | None = None) -> Json: ...

    async def accounts(self, p: Principal, service: Mapping[str, Any]) -> list[Leg]:
        """The legs a request for this service charges, or insufficient_balance (402) when an account is missing."""
        ...

    async def reserve(
        self,
        p: Principal,
        key: str,
        digest: str,
        service: Mapping[str, Any],
        legs: list[Leg],
        deadline: int,
        decision: Mapping[str, Any] | None = None,
    ) -> tuple[RequestRecord, bool]:
        """Admit a request and hold each leg's reserved amount, all or nothing. ``fresh`` is False on a replay."""
        ...

    async def get(self, p: Principal, id: str) -> RequestRecord: ...

    async def begin(self, p: Principal, id: str, offering: Mapping[str, Any], bound: str) -> str:
        """Record a provider dispatch; returns its attempt id."""
        ...

    async def output(self, p: Principal, id: str) -> None:
        """The caller has seen output, so a failure can no longer be refunded."""
        ...

    async def attempt_result(
        self,
        p: Principal,
        id: str,
        attempt: str,
        status: str,
        usage: Mapping[str, Any] | None,
        cost: str | None,
        upstream: str | None = None,
    ) -> None:
        """The first result of an attempt is final; later calls are ignored."""
        ...

    async def finish(
        self, p: Principal, id: str, execution: str, usage: Mapping[str, Any] | None, pending: bool
    ) -> None:
        """Settle or release the reservation, or leave it pending when the outcome is uncertain."""
        ...

    async def cancel(self, p: Principal, id: str) -> None: ...


class DataStore(Store, Protocol):
    """Everything the data-plane server and worker need from storage beyond the router's own Store."""

    async def publish(
        self, p: Principal, service: Mapping[str, Any], mode: str = "self-host", expected_version: int | None = None
    ) -> Json: ...

    async def list_services(self, p: Principal) -> list[Json]: ...

    async def balance(self, p: Principal) -> list[BalanceRow]: ...

    async def grant(
        self,
        p: Principal,
        kind: str,
        asset: str,
        amount: str,
        source: str,
        expires: int | None = None,
        service_ids: list[str] | None = None,
        source_kind: str = "purchased",
    ) -> str:
        """Add a funding bucket. ``source`` is its idempotency key; ``expires`` is epoch ms."""
        ...

    async def disable(self, p: Principal, offering: str) -> None: ...

    async def recover(self) -> int:
        """Settle or release requests whose deadline passed, never redispatching an uncertain attempt."""
        ...


def invocation_bound(invocation: Mapping[str, Any], max_input: int, price: Mapping[str, Any]) -> int:
    """The most one attempt can cost on a price: the token bound for chat, the priced media limits for a task."""
    limits = invocation["limits"]
    if not invocation["input"].get("task"):
        return bound(max_input, limits["max_output_tokens"], price)
    usage = {"input": 0, "cached": 0, "cache_write": 0, "output": 0, **(limits.get("max_usage") or {})}
    return charge(usage, price)


EmitFn = Callable[[StreamEvent], Awaitable[None]]


@dataclass
class _Attempt:
    """What the provider reported during one attempt, kept even when the attempt later fails."""

    usage: Json | None = None
    upstream: str | None = None
    done: bool = False
    completion: str = "succeeded"


@dataclass
class _Run:
    """Mutable state of one execution, shared by the attempt loop and the failure handler."""

    seq: int = 0
    text: list[str] = field(default_factory=list)
    started: bool = False
    usage: Json | None = None
    execution: str = "failed"
    pending: bool = False
    model: str | None = None


class Router:
    """Runs invocations against a Store and a set of providers. Safe for concurrent use inside one event loop."""

    def __init__(
        self, store: Store, providers: Mapping[str, Provider], digest_key: str, clock: Clock = wall_clock
    ) -> None:
        if len(digest_key) < 32:
            raise ValueError("digest key must be at least 32 characters")
        self.store = store
        self.providers = dict(providers)
        self._digest_key = digest_key.encode()
        self._clock = clock

    def _digest(self, invocation: Json) -> str:
        return hmac.new(self._digest_key, canonical(invocation).encode(), hashlib.sha256).hexdigest()

    @staticmethod
    def _validate_shape(inv: Json) -> None:
        task = inv["input"].get("task")
        limits = inv["limits"].get("max_usage")
        if task:
            meter = KIND_METER[task["kind"]]
            limits = limits or {}
            if (
                inv["input"]["messages"]
                or not limits.get(meter)
                or any(k != meter and limits.get(k, 0) != 0 for k in MEDIA_KEYS)
                or (meter == "tasks" and limits.get("tasks") != 1)
            ):
                raise RouterError("invalid_task_limits", 400)
        elif not inv["input"]["messages"] or limits is not None:
            raise RouterError("invalid_request", 400)

    async def infer(
        self,
        principal: Principal,
        raw: Any,
        key: str,
        emit: EmitFn | None = None,
        abort: asyncio.Event | None = None,
    ) -> Json:
        """Run one invocation. ``raw`` is the decoded JSON body; ``key`` is the idempotency key. ``emit`` receives the
        stream events in order; if it raises, the provider is stopped and the outcome is recorded as uncertain.
        Setting ``abort`` (or cancelling the calling task) stops the provider but never the settlement.

        Errors before funds are reserved are plain RouterError; later ones carry ``request_id``."""
        if not principal.has_scope("invoke"):
            raise RouterError("scope_denied", 403)
        inv = parse_invocation(raw)
        self._validate_shape(inv)
        if not principal.may_use(inv["service"]) or inv["customer_id"] != principal.customer:
            raise RouterError("scope_denied", 403)
        if not key or len(key) > 200:
            raise RouterError("missing_idempotency_key", 400)
        digest = self._digest(inv)
        existing = await self.store.replay(principal, key, digest)
        if existing:
            return _replayed(existing)
        service = await self.store.get_service(principal, inv["service"], inv.get("service_version"))
        if "service_version" in inv:
            active = await self.store.get_service(principal, inv["service"])
            if active["version"] != service["version"]:
                raise RouterError("inactive_service_version", 409)
        if inv["limits"]["max_output_tokens"] > service["max_output_tokens"]:
            raise RouterError("unsupported_capability", 422)
        candidates, excluded = route(service, inv)
        if not candidates:
            raise RouterError("no_eligible_offering", 422)
        if any(c.offering["provider"] not in self.providers for c in candidates):
            raise RouterError("provider_unavailable", 503)
        max_input = max(c.offering["max_input_tokens"] for c in candidates)
        legs = await self.store.accounts(principal, service)
        for leg in legs:
            leg.reserved = str(invocation_bound(inv, max_input, leg.price))
            max_charge = inv["limits"].get("max_charge")
            if (
                leg.kind == "customer_sale"
                and max_charge
                and (max_charge["asset"] != leg.price["asset"] or int(max_charge["atoms"]) < int(leg.reserved))
            ):
                raise RouterError("budget_exceeded", 402)
        decision = {
            "excluded": excluded,
            "candidates": [{"id": c.offering["id"], "score": c.score, "cost": str(c.cost)} for c in candidates],
        }
        record, fresh = await self.store.reserve(
            principal, key, digest, service, legs, self._clock() + inv["limits"]["timeout_ms"], decision
        )
        if not fresh:
            return _replayed(record)
        return await self._execute(principal, inv, service, candidates, legs, record, emit, abort)

    async def _execute(
        self,
        p: Principal,
        inv: Json,
        service: Json,
        candidates: list[Candidate],
        legs: list[Leg],
        request: RequestRecord,
        emit: EmitFn | None,
        external: asyncio.Event | None,
    ) -> Json:
        run = _Run()
        stop = asyncio.Event()
        loop = asyncio.get_running_loop()
        timer = loop.call_later(inv["limits"]["timeout_ms"] / 1000, stop.set)
        helpers: list[asyncio.Task[None]] = [asyncio.create_task(self._poll_cancel(p, request.id, stop))]
        if external is not None:
            helpers.append(asyncio.create_task(_relay(external, stop)))

        async def send(kind: str, data: Json | None = None) -> None:
            run.seq += 1
            if emit is not None:
                await emit(StreamEvent(kind, request.id, run.seq, data or {}))

        try:
            await send("request.started", {"service_version": service["version"]})
            await self._attempts(p, inv, service, candidates, request, run, stop, send)
            return await asyncio.shield(self._settle(p, service, legs, request, run, send))
        except asyncio.CancelledError:
            # The caller went away: finish what is certain, keep what is not.
            await asyncio.shield(self._abandon(p, request.id, run))
            raise
        except BaseException as exc:
            await asyncio.shield(self._abandon(p, request.id, run))
            error = exc if isinstance(exc, RouterError) else RouterError("execution_unknown", 503)
            error.request_id = request.id  # type: ignore[attr-defined]
            if error is exc:
                raise
            raise error from exc
        finally:
            timer.cancel()
            for task in helpers:
                task.cancel()
            await asyncio.gather(*helpers, return_exceptions=True)

    async def _poll_cancel(self, p: Principal, request_id: str, stop: asyncio.Event) -> None:
        while not stop.is_set():
            await asyncio.sleep(0.1)
            try:
                if (await self.store.get(p, request_id)).cancel_requested:
                    stop.set()
            except Exception:  # noqa: BLE001 - an unreadable request is treated as cancelled
                stop.set()

    async def _abandon(self, p: Principal, request_id: str, run: _Run) -> None:
        """Unexpected failure: never turn an uncertain dispatch into a refund."""
        with contextlib.suppress(Exception):
            await self.store.finish(p, request_id, "partial" if run.started else "unknown", None, True)

    async def _attempts(
        self,
        p: Principal,
        inv: Json,
        service: Json,
        candidates: list[Candidate],
        request: RequestRecord,
        run: _Run,
        stop: asyncio.Event,
        send: Callable[[str, Json | None], Awaitable[None]],
    ) -> None:
        for candidate in candidates[: service["max_attempts"]]:
            if stop.is_set():
                run.execution = "cancelled"
                return
            offering = candidate.offering
            provider = self.providers.get(offering["provider"])
            if provider is None:
                raise RouterError("provider_unavailable", 503)
            try:
                attempt = await self.store.begin(p, request.id, offering, str(candidate.cost))
            except RouterError as e:
                if e.code in ("budget_exceeded", "connection_disabled", "cancelled"):
                    run.execution = "cancelled" if e.code == "cancelled" else "failed"
                    continue
                raise
            seen = _Attempt()
            run.model = offering["model"]

            async def consume(
                provider: Provider = provider, offering: Mapping[str, Any] = offering, seen: _Attempt = seen
            ) -> None:
                events: AsyncIterator[ProviderEvent] = provider.invoke(inv, offering)
                try:
                    async for event in events:
                        if isinstance(event, Delta):
                            if not run.started:
                                await self.store.output(p, request.id)
                                run.started = True
                            run.text.append(event.text)
                            if sum(len(t.encode()) for t in run.text) > 1_000_000:
                                raise ProviderError(False, False, "output_too_large")
                            await send("output.delta", {"text": event.text})
                        elif isinstance(event, Usage):
                            seen.usage = valid_meters(event.meters)
                            if seen.usage is None:
                                raise ProviderError(False, False, "invalid_usage")
                            seen.upstream = event.upstream_id
                        elif isinstance(event, Done):
                            seen.done, seen.completion = True, event.completion
                finally:
                    aclose = getattr(events, "aclose", None)
                    if aclose is not None:
                        await aclose()

            worker = asyncio.create_task(consume())
            waiter = asyncio.create_task(stop.wait())
            try:
                await asyncio.wait({worker, waiter}, return_when=asyncio.FIRST_COMPLETED)
            except asyncio.CancelledError:
                worker.cancel()
                await asyncio.gather(worker, return_exceptions=True)
                await self._record_attempt(p, request.id, attempt, seen.usage, offering, seen.upstream)
                raise
            finally:
                waiter.cancel()
            failure: BaseException | None = None
            if worker.done():
                try:
                    worker.result()
                except Exception as e:  # noqa: BLE001 - classified below
                    failure = e
            else:
                worker.cancel()
                await asyncio.gather(worker, return_exceptions=True)
                failure = ProviderError(False, False)
            if failure is None:
                try:
                    if not seen.done or seen.usage is None:
                        raise ProviderError(False, False, "usage_unknown")
                    code = _check_usage(inv, seen.usage, seen.completion)
                    if code:
                        seen.usage = None
                        raise ProviderError(False, False, code)
                    cost = charge(seen.usage, offering["price"])
                    await self.store.attempt_result(
                        p, request.id, attempt, seen.completion, seen.usage, str(cost), seen.upstream
                    )
                    run.usage = seen.usage
                    run.execution = seen.completion
                    run.pending = cost > candidate.cost
                    return
                except Exception as e:  # noqa: BLE001 - classified below
                    failure = e
            definitive = isinstance(failure, ProviderError) and failure.definitive and not run.started
            await self._record_attempt(p, request.id, attempt, seen.usage, offering, seen.upstream, definitive)
            if not definitive:
                run.pending = True
                run.execution = "partial" if run.started else "unknown"
                return
            if not (isinstance(failure, ProviderError) and failure.retryable):
                return

    async def _record_attempt(
        self,
        p: Principal,
        request_id: str,
        attempt: str,
        usage: Json | None,
        offering: Mapping[str, Any],
        upstream: str | None,
        definitive: bool = False,
    ) -> None:
        cost = str(charge(usage, offering["price"])) if usage is not None else "0" if definitive else None
        await self.store.attempt_result(
            p, request_id, attempt, "failed" if definitive else "unknown", usage, cost, upstream
        )

    async def _settle(
        self,
        p: Principal,
        service: Json,
        legs: list[Leg],
        request: RequestRecord,
        run: _Run,
        send: Callable[[str, Json | None], Awaitable[None]],
    ) -> Json:
        await self.store.finish(p, request.id, run.execution, run.usage, run.pending)
        settled = await self.store.get(p, request.id)
        await send(
            "request.completed" if run.execution == "succeeded" else "request.failed",
            {"execution_status": run.execution, "billing_status": settled.billing},
        )
        usage = run.usage
        if usage is not None:
            usage_report: Json = {"status": "confirmed", "meters": usage}
        else:
            usage_report = {"status": "not_billable" if settled.billing == "released" else "pending"}
        charges = []
        if usage is not None and settled.billing == "settled":
            charges = [
                {"kind": leg.kind, "asset": leg.price["asset"], "atoms": str(charge(usage, leg.price)), "scale": 9}
                for leg in legs
                if leg.kind == "customer_sale" or p.has_scope("billing:read")
            ]
        result: Json = {
            "id": request.id,
            "execution_status": run.execution,
            "billing_status": settled.billing,
            "service_version": service["version"],
        }
        if run.model is not None:
            result["model"] = run.model
        if "tier" in service:
            result["tier"] = service["tier"]
        result["output"] = {"role": "assistant", "content": "".join(run.text)}
        result["usage"] = usage_report
        result["billing"] = {"charges": charges}
        return result


async def _relay(source: asyncio.Event, target: asyncio.Event) -> None:
    await source.wait()
    target.set()


def _replayed(record: RequestRecord) -> Json:
    return {
        "replayed": True,
        "id": record.id,
        "execution_status": record.execution,
        "billing_status": record.billing,
    }


def _check_usage(inv: Json, usage: Json, completion: str) -> str:
    """Per-kind limits on what a provider reported. Returns an error code, or "" when the usage is fine."""
    task = inv["input"].get("task")
    if task:
        meter = KIND_METER[task["kind"]]
        limits = inv["limits"].get("max_usage") or {}
        if any(usage[k] != 0 for k in ("input", "cached", "cache_write", "output")):
            return "usage_out_of_bounds"
        if any(usage.get(k, 0) > limits.get(k, 0) for k in MEDIA_KEYS):
            return "usage_out_of_bounds"
        if meter == "tasks" and usage.get("tasks") != (1 if completion == "succeeded" else 0):
            return "usage_out_of_bounds"
        return ""
    if any(usage.get(k, 0) != 0 for k in MEDIA_KEYS):
        return "unexpected_meter"
    return ""


class MockProvider:
    """A deterministic provider for tests and local runs. ``behavior`` is "success", "reject" (a definitive,
    retryable rejection), "unknown" (outcome lost before any output) or "partial" (output, then loss)."""

    def __init__(self, behavior: str = "success", delay: float = 0.0) -> None:
        self.behavior = behavior
        self.delay = delay
        self.calls = 0

    async def invoke(self, invocation: Mapping[str, Any], offering: Mapping[str, Any]) -> AsyncIterator[ProviderEvent]:
        self.calls += 1
        if invocation["input"].get("task"):
            raise ProviderError(True, False, "unsupported_task")
        if self.delay:
            await asyncio.sleep(self.delay)
        if self.behavior == "reject":
            raise ProviderError(True, True, "rate_limited")
        if self.behavior == "unknown":
            raise ProviderError(False, False)
        yield Delta("Hello from SuperRouter.")
        if self.behavior == "partial":
            raise ProviderError(False, False)
        output = min(5, invocation["limits"]["max_output_tokens"])
        meters = {"input": 10, "cached": 0, "cache_write": 0, "output": output}
        yield Usage(meters, str(uuid.uuid4()))
        yield Done()


class MockTaskProvider:
    """Simulates bounded media tasks and produces a receipt, never real media."""

    def __init__(self, behavior: str = "success") -> None:
        self.behavior = behavior
        self.calls = 0

    async def invoke(self, invocation: Mapping[str, Any], offering: Mapping[str, Any]) -> AsyncIterator[ProviderEvent]:
        self.calls += 1
        task = invocation["input"].get("task")
        if not task or task["kind"] != offering.get("task_kind") or self.behavior == "reject":
            raise ProviderError(True, False, "unsupported_task")
        if self.behavior == "unknown":
            raise ProviderError(False, False, "usage_unknown")
        meter = KIND_METER[task["kind"]]
        units = (invocation["limits"].get("max_usage") or {}).get(meter, 0)
        if not units or units > (offering.get("max_usage") or {}).get(meter, 0):
            raise ProviderError(True, False, "meter_limit")
        partial = self.behavior == "partial"
        completed = (0 if meter == "tasks" else units // 2) if partial else units
        yield Delta(f"Simulated {task['kind']} task receipt; no media generated.")
        yield Usage({"input": 0, "cached": 0, "cache_write": 0, "output": 0, meter: completed}, str(uuid.uuid4()))
        yield Done("partial" if partial else "succeeded")
