# SPDX-License-Identifier: Apache-2.0
"""Reference in-memory implementation of :class:`~super_router.router.DataStore`.

It follows the PostgreSQL store's semantics exactly: idempotent admission, bucketed holds, a provider cost
budget, a balanced journal and an append-only outbox. Every method runs to completion without awaiting, so each
one is atomic inside one event loop. State is lost on restart, which makes it right for tests, demos and
single-process self-hosting without a database.
"""

from __future__ import annotations

import copy
import json
import re
import uuid
from collections.abc import Mapping
from dataclasses import dataclass, field
from typing import Any

from ..contracts import Json, Principal, parse_service
from ..pricing import RouterError, charge
from ..router import BalanceRow, Clock, Leg, RequestRecord, wall_clock

_AMOUNT = re.compile(r"[1-9][0-9]{0,29}")
_RANK = {"subscription": 0, "promotion": 1}


@dataclass
class _Account:
    id: str
    owner: str
    kind: str
    asset: str
    balance: int = 0
    held: int = 0


@dataclass
class _Bucket:
    id: str
    seq: int
    account: str
    source: str
    expires_at: int | None
    remaining: int
    granted: int
    service_ids: list[str] | None
    source_kind: str
    held: int = 0

    def order(self) -> tuple[float, int, int]:
        return (float("inf") if self.expires_at is None else self.expires_at, _RANK.get(self.source_kind, 2), self.seq)


@dataclass
class _Allocation:
    bucket: str
    account: str
    amount: int
    charged: int = 0


@dataclass
class _Attempt:
    id: str
    offering: str
    bound: int
    status: str = "dispatched"
    usage: Json | None = None
    cost: int | None = None
    upstream: str | None = None


@dataclass
class _Request:
    record: RequestRecord
    digest: str
    provider_spent: int = 0
    provider_held: int = 0
    attempts: list[_Attempt] = field(default_factory=list)
    allocations: list[_Allocation] = field(default_factory=list)


@dataclass
class _Tenant:
    workspace: str
    environment: str
    services: dict[str, list[Json]] = field(default_factory=dict)
    active: dict[str, int] = field(default_factory=dict)
    accounts: dict[tuple[str, str, str], _Account] = field(default_factory=dict)
    accounts_by_id: dict[str, _Account] = field(default_factory=dict)
    buckets: list[_Bucket] = field(default_factory=list)
    requests: dict[str, _Request] = field(default_factory=dict)
    idem: dict[str, str] = field(default_factory=dict)
    disabled: set[str] = field(default_factory=set)
    journal: list[Json] = field(default_factory=list)
    outbox: list[Json] = field(default_factory=list)
    reconciliation: dict[str, str] = field(default_factory=dict)
    entitlements: list[Json] = field(default_factory=list)


def _same(a: Any, b: Any) -> bool:
    return json.dumps(a, sort_keys=True) == json.dumps(b, sort_keys=True)


class MemoryStore:
    """In-memory DataStore. Pass a ``clock`` (epoch ms) to control time in tests."""

    def __init__(self, clock: Clock = wall_clock) -> None:
        self._clock = clock
        self._tenants: dict[tuple[str, str], _Tenant] = {}
        self._seq = 0

    def _tenant(self, p: Principal) -> _Tenant:
        key = (p.workspace, p.environment)
        if key not in self._tenants:
            self._tenants[key] = _Tenant(p.workspace, p.environment)
        return self._tenants[key]

    def _id(self, prefix: str) -> str:
        self._seq += 1
        return f"{prefix}{self._seq:08d}"

    @staticmethod
    def _row(t: _Tenant, p: Principal, id: str) -> _Request:
        r = t.requests.get(id)
        if r is None or r.record.customer != p.customer:
            raise RouterError("resource_not_found", 404)
        if not p.may_use(r.record.service["id"]):
            raise RouterError("scope_denied", 403)
        return r

    @staticmethod
    def _view(r: _Request) -> RequestRecord:
        """What callers see: a copy, so nothing can mutate store state."""
        rec = r.record
        return RequestRecord(
            rec.id,
            rec.workspace,
            rec.environment,
            rec.customer,
            rec.service,
            rec.execution,
            rec.billing,
            rec.cancel_requested,
            rec.output_started,
            rec.deadline,
            [Leg(**vars(leg)) for leg in rec.legs],
            str(r.provider_spent),
        )

    async def replay(self, p: Principal, key: str, digest: str) -> RequestRecord | None:
        t = self._tenant(p)
        id = t.idem.get(key)
        if id is None:
            return None
        r = t.requests[id]
        if r.digest != digest or r.record.customer != p.customer:
            raise RouterError("idempotency_conflict", 409)
        return self._view(r)

    async def get(self, p: Principal, id: str) -> RequestRecord:
        return self._view(self._row(self._tenant(p), p, id))

    async def publish(
        self, p: Principal, service: Mapping[str, Any], mode: str = "self-host", expected_version: int | None = None
    ) -> Json:
        if not p.has_scope("service:write"):
            raise RouterError("scope_denied", 403)
        s = parse_service(service)
        if not p.may_use(s["id"]):
            raise RouterError("scope_denied", 403)
        if mode == "cloud" and any(o["source"] == "byok" for o in s["offerings"]):
            raise RouterError("byok_not_allowed", 403)
        t = self._tenant(p)
        versions = t.services.get(s["id"], [])
        previous = versions[-1] if versions else None
        if expected_version is not None and (previous is None or previous["version"] != expected_version):
            raise RouterError("service_version_conflict", 409)
        protected = mode == "cloud" and p.auth_key_id
        if protected and (
            previous is None
            or not _same(previous.get("platform_price"), s.get("platform_price"))
            or not _same(previous["offerings"], s["offerings"])
        ):
            raise RouterError("platform_pricing_protected", 403)
        for existing in versions:
            if existing["version"] == s["version"]:
                if not _same(existing, s):
                    raise RouterError("version_exists", 409)
                return s
        if previous is not None and s["version"] <= previous["version"]:
            raise RouterError("service_version_conflict", 409)
        t.services.setdefault(s["id"], []).append(copy.deepcopy(s))
        t.active[s["id"]] = s["version"]
        return s

    async def get_service(self, p: Principal, id: str, version: int | None = None) -> Json:
        if not p.may_use(id):
            raise RouterError("scope_denied", 403)
        t = self._tenant(p)
        wanted = version if version is not None else t.active.get(id)
        for s in t.services.get(id, []):
            if s["version"] == wanted:
                return copy.deepcopy(s)
        raise RouterError("resource_not_found", 404)

    async def list_services(self, p: Principal) -> list[Json]:
        t = self._tenant(p)
        out: list[Json] = []
        for id in sorted(t.active)[:100]:
            if p.may_use(id):
                out.extend(copy.deepcopy(s) for s in t.services[id] if s["version"] == t.active[id])
        return out

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
        if not p.has_scope("billing:write"):
            raise RouterError("scope_denied", 403)
        if _AMOUNT.fullmatch(amount) is None:
            raise RouterError("invalid_amount", 400)
        t = self._tenant(p)
        owner = p.workspace if kind == "platform_sale" else p.customer
        for prior in t.buckets:
            if prior.source == source:
                a = t.accounts_by_id[prior.account]
                if (
                    str(prior.granted) != amount
                    or a.asset != asset
                    or a.owner != owner
                    or a.kind != kind
                    or prior.expires_at != expires
                    or prior.service_ids != service_ids
                    or prior.source_kind != source_kind
                ):
                    raise RouterError("idempotency_conflict", 409)
                return a.id
        account = t.accounts.get((owner, kind, asset))
        if account is None:
            account = _Account(self._id("acct_"), owner, kind, asset)
            t.accounts[(owner, kind, asset)] = account
        t.accounts_by_id[account.id] = account
        account.balance += int(amount)
        self._seq += 1
        t.buckets.append(
            _Bucket(
                self._id("bkt_"),
                self._seq,
                account.id,
                source,
                expires,
                int(amount),
                int(amount),
                service_ids,
                source_kind,
            )
        )
        t.journal.append(
            {
                "key": f"grant:{source}",
                "asset": asset,
                "debit": f"issuer:{asset}",
                "credit": account.id,
                "amount": amount,
                "reason": "grant",
            }
        )
        return account.id

    async def accounts(self, p: Principal, service: Mapping[str, Any]) -> list[Leg]:
        t = self._tenant(p)
        legs = []
        for kind, price in (("customer_sale", service["price"]), ("platform_sale", service.get("platform_price"))):
            if not price:
                continue
            owner = p.workspace if kind == "platform_sale" else p.customer
            account = t.accounts.get((owner, kind, price["asset"]))
            if account is None:
                raise RouterError("insufficient_balance", 402)
            legs.append(Leg(kind, account.id, price, "0"))
        return legs

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
        if not p.may_use(service["id"]):
            raise RouterError("scope_denied", 403)
        t = self._tenant(p)
        now = self._clock()
        if service.get("requires_subscription") and not any(
            e["customer"] == p.customer and service["id"] in e["service_ids"] and e["start"] <= now < e["end"]
            for e in t.entitlements
        ):
            raise RouterError("subscription_required", 403)
        old_id = t.idem.get(key)
        if old_id is not None:
            old = t.requests[old_id]
            if old.digest != digest or old.record.customer != p.customer:
                raise RouterError("idempotency_conflict", 409)
            return self._view(old), False
        if t.active.get(service["id"]) != service["version"]:
            raise RouterError("inactive_service_version", 409)
        # Compute every allocation first, then apply: a failure leaves no partial hold.
        taken: list[tuple[_Bucket, int]] = []
        held: dict[str, int] = {}
        for leg in sorted(legs, key=lambda x: x.account):
            remaining = int(leg.reserved)
            if leg.account not in t.accounts_by_id:
                raise RouterError("insufficient_balance", 402)
            usable = [
                b
                for b in t.buckets
                if b.account == leg.account
                and (b.expires_at is None or b.expires_at > now)
                and (b.service_ids is None or service["id"] in b.service_ids)
            ]
            for b in sorted(usable, key=_Bucket.order):
                take = min(b.remaining - b.held - held.get(b.id, 0), remaining)
                if take <= 0:
                    continue
                held[b.id] = held.get(b.id, 0) + take
                taken.append((b, take))
                remaining -= take
                if not remaining:
                    break
            if remaining > 0:
                raise RouterError("insufficient_balance", 402)
        id = str(uuid.uuid4())
        record = RequestRecord(
            id,
            p.workspace,
            p.environment,
            p.customer,
            copy.deepcopy(dict(service)),
            "accepted",
            "reserved",
            False,
            False,
            deadline,
            [Leg(**vars(leg)) for leg in legs],
            "0",
        )
        request = _Request(record, digest)
        for bucket, amount in taken:
            bucket.held += amount
            request.allocations.append(_Allocation(bucket.id, bucket.account, amount))
        for leg in legs:
            t.accounts_by_id[leg.account].held += int(leg.reserved)
        t.requests[id] = request
        t.idem[key] = id
        return self._view(request), True

    async def begin(self, p: Principal, id: str, offering: Mapping[str, Any], bound: str) -> str:
        t = self._tenant(p)
        r = self._row(t, p, id)
        if r.record.cancel_requested or self._clock() > r.record.deadline:
            raise RouterError("cancelled", 409)
        if r.record.billing != "reserved":
            raise RouterError("state_conflict", 409)
        if offering["id"] in t.disabled:
            raise RouterError("connection_disabled", 409)
        snapshot = next((o for o in r.record.service["offerings"] if o["id"] == offering["id"]), None)
        if snapshot is None or not _same(snapshot, offering):
            raise RouterError("state_conflict", 409)
        if r.provider_spent + r.provider_held + int(bound) > int(r.record.service["max_provider_cost"]):
            raise RouterError("budget_exceeded", 402)
        attempt = _Attempt(str(uuid.uuid4()), offering["id"], int(bound))
        r.attempts.append(attempt)
        r.record.execution = "running"
        r.provider_held += int(bound)
        return attempt.id

    async def output(self, p: Principal, id: str) -> None:
        self._row(self._tenant(p), p, id).record.output_started = True

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
        r = self._row(self._tenant(p), p, id)
        if r.record.billing not in ("reserved", "pending"):
            raise RouterError("state_conflict", 409)
        a = next((x for x in r.attempts if x.id == attempt), None)
        if a is None:
            raise RouterError("resource_not_found", 404)
        if a.status != "dispatched":
            return
        a.status, a.usage, a.upstream = status, dict(usage) if usage is not None else None, upstream
        a.cost = None if cost is None else int(cost)
        if a.cost is not None:
            r.provider_spent += a.cost
            r.provider_held -= a.bound

    async def finish(
        self, p: Principal, id: str, execution: str, usage: Mapping[str, Any] | None, pending: bool
    ) -> None:
        t = self._tenant(p)
        self._finish(t, self._row(t, p, id), execution, usage, pending)

    def _event(self, t: _Tenant, id: str, kind: str) -> None:
        t.outbox.append({"id": str(uuid.uuid4()), "type": kind, "request": id})

    def _finish(self, t: _Tenant, r: _Request, execution: str, usage: Mapping[str, Any] | None, pending: bool) -> None:
        rec = r.record
        if rec.billing in ("settled", "released"):
            return
        if pending and not rec.output_started and not r.attempts:
            pending, execution = False, "cancelled"
        if usage is not None and any(a.cost is not None and a.cost > a.bound for a in r.attempts):
            pending = True
        due = [charge(usage, leg.price) if usage is not None else 0 for leg in rec.legs]
        exceeded = any(d > int(leg.reserved) for d, leg in zip(due, rec.legs, strict=True))
        if pending or exceeded:
            if rec.billing != "pending":
                self._event(t, rec.id, "reconciliation.required")
            rec.execution, rec.billing = execution, "pending"
            t.reconciliation.setdefault(rec.id, "unknown_outcome" if pending else "reservation_exceeded")
            return
        for leg, amount in zip(rec.legs, due, strict=True):
            remaining = amount
            mine = [
                (a, next(b for b in t.buckets if b.id == a.bucket)) for a in r.allocations if a.account == leg.account
            ]
            for a, b in sorted(mine, key=lambda pair: pair[1].order()):
                take = min(remaining, a.amount)
                remaining -= take
                a.charged = take
                b.remaining -= take
                b.held -= a.amount
            account = t.accounts_by_id[leg.account]
            account.balance -= amount
            account.held -= int(leg.reserved)
            t.journal.append(
                {
                    "key": f"settle:{rec.id}:{leg.kind}",
                    "asset": leg.price["asset"],
                    "debit": leg.account,
                    "credit": f"consumed:{leg.price['asset']}",
                    "amount": str(amount),
                    "reason": "settlement",
                }
            )
        rec.execution = execution
        rec.billing = "settled" if usage is not None else "released"
        self._expire(t)
        self._event(t, rec.id, "billing.settled" if usage is not None else "billing.released")

    def _expire(self, t: _Tenant) -> None:
        """Move the unheld remainder of expired buckets out of the balance."""
        now = self._clock()
        for b in t.buckets:
            if b.expires_at is None or b.expires_at > now or b.remaining <= b.held:
                continue
            n = b.remaining - b.held
            b.remaining = b.held
            account = t.accounts_by_id[b.account]
            account.balance -= n
            t.journal.append(
                {
                    "key": self._id("expire:x_"),
                    "asset": account.asset,
                    "debit": b.account,
                    "credit": f"expired:{account.asset}",
                    "amount": str(n),
                    "reason": "expiry",
                }
            )

    async def balance(self, p: Principal) -> list[BalanceRow]:
        t = self._tenant(p)
        self._expire(t)
        rows = [
            a
            for a in t.accounts.values()
            if a.owner == p.customer or (a.owner == p.workspace and a.kind == "platform_sale")
        ]
        rows.sort(key=lambda a: (a.kind, a.id))
        return [BalanceRow(a.id, a.kind, a.asset, str(a.balance), str(a.held), str(a.balance - a.held)) for a in rows]

    async def cancel(self, p: Principal, id: str) -> None:
        self._row(self._tenant(p), p, id).record.cancel_requested = True

    async def disable(self, p: Principal, offering: str) -> None:
        if not p.has_scope("service:write"):
            raise RouterError("scope_denied", 403)
        self._tenant(p).disabled.add(offering)

    async def recover(self) -> int:
        """Never redispatch an expired uncertain attempt."""
        n = 0
        now = self._clock()
        for t in self._tenants.values():
            for r in list(t.requests.values()):
                if r.record.billing != "reserved" or r.record.deadline >= now:
                    continue
                last = r.attempts[-1] if r.attempts else None
                known = last is not None and last.status in ("succeeded", "partial") and last.usage is not None
                if known and last is not None:
                    execution, usage = last.status, last.usage
                else:
                    execution, usage = ("unknown" if r.attempts else "cancelled"), None
                self._finish(t, r, execution, usage, bool(r.attempts) and not known)
                n += 1
        return n

    def entitle(self, p: Principal, service_ids: list[str], start: int, end: int) -> None:
        """Entitle a customer to subscription-only services for a period (epoch ms)."""
        self._tenant(p).entitlements.append(
            {"customer": p.customer, "service_ids": service_ids, "start": start, "end": end}
        )

    def inspect(self, p: Principal) -> Json:
        """A read-only snapshot for tests and diagnostics."""
        t = self._tenant(p)
        return {
            "journal": copy.deepcopy(t.journal),
            "outbox": copy.deepcopy(t.outbox),
            "reconciliation": dict(t.reconciliation),
            "buckets": [{"source": b.source, "remaining": str(b.remaining), "held": str(b.held)} for b in t.buckets],
        }

    def request_ids(self, p: Principal) -> list[str]:
        return sorted(self._tenant(p).requests)
