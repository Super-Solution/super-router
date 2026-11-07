# SPDX-License-Identifier: Apache-2.0
"""PostgreSQL implementation of :class:`~super_router.router.DataStore`.

It talks to the database through the small :class:`~super_router.stores.sql.Database` port, so psycopg, asyncpg or
an ORM's pool all work. The schema is shared with the TypeScript and Go stores, so all three can serve the same
database. Every operation runs in one transaction that first takes an advisory lock on the tenant (workspace and
environment), which serialises admission, holds and settlement per tenant.
"""

from __future__ import annotations

import json
import re
import uuid
from collections.abc import Awaitable, Callable, Mapping
from importlib import resources
from typing import Any

from ..contracts import Json, Principal, parse_service
from ..pricing import RouterError, charge
from ..router import BalanceRow, Clock, Leg, RequestRecord, wall_clock
from .sql import Database, Row, Tx

_AMOUNT = re.compile(r"[1-9][0-9]{0,29}")
_COLUMNS = (
    "id,customer,digest,service,legs,execution,billing,cancel_requested,output_started,provider_spent::text AS spent,"
    "(extract(epoch from deadline)*1000)::bigint AS deadline_ms"
)
_ARRAY = "(SELECT array_agg(x) FROM jsonb_array_elements_text({}::jsonb) x)"
_MS = "(timestamptz 'epoch' + ({}::bigint) * interval '1 millisecond')"

Revalidate = Callable[[Tx, Principal, str], Awaitable[None]]


def _same(a: Any, b: Any) -> bool:
    return json.dumps(a, sort_keys=True) == json.dumps(b, sort_keys=True)


def _json(value: Any) -> str:
    return json.dumps(value, separators=(",", ":"))


def _load(value: Any) -> Any:
    return json.loads(value) if isinstance(value, (str, bytes)) else value


class PostgresStore:
    """DataStore on PostgreSQL.

    ``revalidate`` is called inside each write transaction before it changes anything, with the scope the operation
    needs ("service:write", "billing:write" or "invoke"). Hosts that authenticate with database-backed keys use it
    to re-check that the key was not revoked or narrowed since it was verified.
    """

    def __init__(self, db: Database, clock: Clock = wall_clock, revalidate: Revalidate | None = None) -> None:
        self.db = db
        self._clock = clock
        self._revalidate = revalidate

    async def migrate(self) -> None:
        """Apply the migrations that have not run yet, under an advisory lock so several instances can start at once.
        The files are byte-identical to the TypeScript and Go stores'."""
        async with self.db.transaction() as tx:
            await tx.execute("SELECT pg_advisory_xact_lock(hashtextextended('superrouter:migrate',0))")
            await tx.execute("CREATE TABLE IF NOT EXISTS sr_migrations(version integer PRIMARY KEY)")
            files = sorted(
                (f for f in resources.files(__package__ + ".migrations").iterdir() if f.name.endswith(".sql")),
                key=lambda f: f.name,
            )
            for file in files:
                version = int(file.name.split("_", 1)[0])
                if await tx.fetch_one("SELECT 1 AS done FROM sr_migrations WHERE version=$1", version):
                    continue
                await tx.execute(file.read_text(encoding="utf-8"))
                await tx.execute("INSERT INTO sr_migrations VALUES($1)", version)

    def _tx(self, p: Principal) -> _Scoped:
        return _Scoped(self.db, p)

    async def _check(self, tx: Tx, p: Principal, scope: str) -> None:
        if self._revalidate is not None:
            await self._revalidate(tx, p, scope)
        if scope and not p.has_scope(scope):
            raise RouterError("scope_denied", 403)

    @staticmethod
    def _record(p_workspace: str, p_environment: str, row: Row) -> RequestRecord:
        return RequestRecord(
            id=row["id"],
            workspace=p_workspace,
            environment=p_environment,
            customer=row["customer"],
            service=parse_service(_load(row["service"])),
            execution=row["execution"],
            billing=row["billing"],
            cancel_requested=row["cancel_requested"],
            output_started=row["output_started"],
            deadline=int(row["deadline_ms"]),
            legs=[Leg.from_json(leg) for leg in _load(row["legs"])],
            provider_spent=row["spent"],
        )

    async def _row(self, tx: Tx, p: Principal, id: str) -> RequestRecord:
        row = await tx.fetch_one(
            f"SELECT {_COLUMNS} FROM sr_requests WHERE workspace=$1 AND environment=$2 AND id=$3 AND customer=$4",
            p.workspace,
            p.environment,
            id,
            p.customer,
        )
        if row is None:
            raise RouterError("resource_not_found", 404)
        record = self._record(p.workspace, p.environment, row)
        if not p.may_use(record.service["id"]):
            raise RouterError("scope_denied", 403)
        return record

    async def replay(self, p: Principal, key: str, digest: str) -> RequestRecord | None:
        async with self._tx(p) as tx:
            row = await tx.fetch_one(
                f"SELECT {_COLUMNS} FROM sr_requests WHERE workspace=$1 AND environment=$2 AND idem=$3",
                p.workspace,
                p.environment,
                key,
            )
        if row is None:
            return None
        if row["digest"] != digest or row["customer"] != p.customer:
            raise RouterError("idempotency_conflict", 409)
        return self._record(p.workspace, p.environment, row)

    async def get(self, p: Principal, id: str) -> RequestRecord:
        async with self._tx(p) as tx:
            return await self._row(tx, p, id)

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
        async with self._tx(p) as tx:
            await self._check(tx, p, "service:write")
            latest = await tx.fetch_one(
                "SELECT config FROM sr_services WHERE workspace=$1 AND environment=$2 AND id=$3"
                " ORDER BY version DESC LIMIT 1",
                p.workspace,
                p.environment,
                s["id"],
            )
            previous = parse_service(_load(latest["config"])) if latest else None
            if expected_version is not None and (previous is None or previous["version"] != expected_version):
                raise RouterError("service_version_conflict", 409)
            if (
                mode == "cloud"
                and p.auth_key_id
                and (
                    previous is None
                    or not _same(previous.get("platform_price"), s.get("platform_price"))
                    or not _same(previous["offerings"], s["offerings"])
                )
            ):
                # Database-authenticated developers cannot provision or rewrite platform supply economics.
                raise RouterError("platform_pricing_protected", 403)
            same_version = await tx.fetch_one(
                "SELECT config FROM sr_services WHERE workspace=$1 AND environment=$2 AND id=$3 AND version=$4",
                p.workspace,
                p.environment,
                s["id"],
                s["version"],
            )
            if same_version:
                if not _same(parse_service(_load(same_version["config"])), s):
                    raise RouterError("version_exists", 409)
                return s
            if previous is not None and s["version"] <= previous["version"]:
                raise RouterError("service_version_conflict", 409)
            await tx.execute(
                "UPDATE sr_services SET active=false WHERE workspace=$1 AND environment=$2 AND id=$3",
                p.workspace,
                p.environment,
                s["id"],
            )
            await tx.execute(
                "INSERT INTO sr_services(workspace,environment,id,version,config) VALUES($1,$2,$3,$4,$5::jsonb)",
                p.workspace,
                p.environment,
                s["id"],
                s["version"],
                _json(s),
            )
            await tx.execute(
                "INSERT INTO sr_audit(id,workspace,environment,actor,action,resource,details)"
                " VALUES($1,$2,$3,$4,$5,$6,$7::jsonb)",
                str(uuid.uuid4()),
                p.workspace,
                p.environment,
                p.member_id or "trusted-host",
                "service.published",
                s["id"],
                _json({"version": s["version"]}),
            )
        return s

    async def get_service(self, p: Principal, id: str, version: int | None = None) -> Json:
        if not p.may_use(id):
            raise RouterError("scope_denied", 403)
        async with self._tx(p) as tx:
            if version is None:
                row = await tx.fetch_one(
                    "SELECT config FROM sr_services WHERE workspace=$1 AND environment=$2 AND id=$3 AND active=true",
                    p.workspace,
                    p.environment,
                    id,
                )
            else:
                row = await tx.fetch_one(
                    "SELECT config FROM sr_services WHERE workspace=$1 AND environment=$2 AND id=$3 AND version=$4",
                    p.workspace,
                    p.environment,
                    id,
                    version,
                )
        if row is None:
            raise RouterError("resource_not_found", 404)
        return parse_service(_load(row["config"]))

    async def list_services(self, p: Principal) -> list[Json]:
        async with self._tx(p) as tx:
            rows = await tx.fetch_all(
                "SELECT config FROM sr_services WHERE workspace=$1 AND environment=$2 AND active=true"
                " ORDER BY id LIMIT 100",
                p.workspace,
                p.environment,
            )
        services = [parse_service(_load(r["config"])) for r in rows]
        return [s for s in services if p.may_use(s["id"])]

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
        async with self._tx(p) as tx:
            await self._check(tx, p, "billing:write")
            return await self.grant_in(tx, p, kind, asset, amount, source, expires, service_ids, source_kind)

    async def grant_in(
        self,
        tx: Tx,
        p: Principal,
        kind: str,
        asset: str,
        amount: str,
        source: str,
        expires: int | None = None,
        service_ids: list[str] | None = None,
        source_kind: str = "purchased",
    ) -> str:
        """Add a funding bucket inside a transaction the caller owns, after the caller has authorized it.
        Control planes use it to grant credits together with their own writes."""
        owner = p.workspace if kind == "platform_sale" else p.customer
        prior = await tx.fetch_one(
            "SELECT b.account,a.asset,a.owner,a.kind,b.granted::text AS granted,"
            " (extract(epoch from b.expires_at)*1000)::bigint AS expires_ms,"
            " to_json(b.service_ids)::text AS service_ids,b.source_kind"
            " FROM sr_buckets b JOIN sr_accounts a"
            " ON (a.workspace,a.environment,a.id)=(b.workspace,b.environment,b.account)"
            " WHERE b.workspace=$1 AND b.environment=$2 AND b.source=$3",
            p.workspace,
            p.environment,
            source,
        )
        if prior is not None:
            stored_ids = _load(prior["service_ids"]) if prior["service_ids"] else None
            if (
                prior["granted"] != amount
                or prior["asset"] != asset
                or prior["owner"] != owner
                or prior["kind"] != kind
                or prior["expires_ms"] != expires
                or stored_ids != service_ids
                or prior["source_kind"] != source_kind
            ):
                raise RouterError("idempotency_conflict", 409)
            return str(prior["account"])
        await tx.execute(
            "INSERT INTO sr_accounts(workspace,environment,id,owner,kind,asset) VALUES($1,$2,$3,$4,$5,$6)"
            " ON CONFLICT(workspace,environment,owner,kind,asset) DO NOTHING",
            p.workspace,
            p.environment,
            str(uuid.uuid4()),
            owner,
            kind,
            asset,
        )
        account = await tx.fetch_one(
            "SELECT id FROM sr_accounts WHERE workspace=$1 AND environment=$2 AND owner=$3 AND kind=$4 AND asset=$5",
            p.workspace,
            p.environment,
            owner,
            kind,
            asset,
        )
        assert account is not None
        account_id = str(account["id"])
        await tx.execute(
            "UPDATE sr_accounts SET balance=balance+$4::numeric WHERE workspace=$1 AND environment=$2 AND id=$3",
            p.workspace,
            p.environment,
            account_id,
            amount,
        )
        expires_sql = "NULL" if expires is None else _MS.format("$6")
        await tx.execute(
            "INSERT INTO sr_buckets(workspace,environment,id,account,source,expires_at,remaining,granted,service_ids,"
            f"source_kind) VALUES($1,$2,$3,$4,$5,{expires_sql},$7::numeric,$7::numeric,{_ARRAY.format('$8')},$9)",
            p.workspace,
            p.environment,
            str(uuid.uuid4()),
            account_id,
            source,
            expires,
            amount,
            None if service_ids is None else _json(service_ids),
            source_kind,
        )
        await tx.execute(
            "INSERT INTO sr_journal(workspace,environment,business_key,asset,debit,credit,amount,reason)"
            " VALUES($1,$2,$3,$4,$5,$6,$7::numeric,$8)",
            p.workspace,
            p.environment,
            f"grant:{source}",
            asset,
            f"issuer:{asset}",
            account_id,
            amount,
            "grant",
        )
        return account_id

    async def accounts(self, p: Principal, service: Mapping[str, Any]) -> list[Leg]:
        legs: list[Leg] = []
        async with self._tx(p) as tx:
            for kind, price in (("customer_sale", service["price"]), ("platform_sale", service.get("platform_price"))):
                if not price:
                    continue
                owner = p.workspace if kind == "platform_sale" else p.customer
                row = await tx.fetch_one(
                    "SELECT id FROM sr_accounts WHERE workspace=$1 AND environment=$2 AND owner=$3 AND kind=$4"
                    " AND asset=$5",
                    p.workspace,
                    p.environment,
                    owner,
                    kind,
                    price["asset"],
                )
                if row is None:
                    raise RouterError("insufficient_balance", 402)
                legs.append(Leg(kind, str(row["id"]), dict(price), "0"))
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
        async with self._tx(p) as tx:
            await self._check(tx, p, "invoke")
            if not p.may_use(service["id"]):
                raise RouterError("scope_denied", 403)
            if service.get("requires_subscription") and not await tx.fetch_one(
                "SELECT 1 AS ok FROM sr_entitlements WHERE workspace=$1 AND environment=$2 AND customer=$3"
                " AND $4=ANY(service_ids) AND period_start<=now() AND period_end>now() LIMIT 1",
                p.workspace,
                p.environment,
                p.customer,
                service["id"],
            ):
                raise RouterError("subscription_required", 403)
            old = await tx.fetch_one(
                f"SELECT {_COLUMNS} FROM sr_requests WHERE workspace=$1 AND environment=$2 AND idem=$3",
                p.workspace,
                p.environment,
                key,
            )
            if old is not None:
                if old["digest"] != digest or old["customer"] != p.customer:
                    raise RouterError("idempotency_conflict", 409)
                return self._record(p.workspace, p.environment, old), False
            # Admission and service publication share the tenant lock. Replays keep their admitted snapshot;
            # fresh work must reserve against the active version.
            if not await tx.fetch_one(
                "SELECT 1 AS ok FROM sr_services WHERE workspace=$1 AND environment=$2 AND id=$3 AND version=$4"
                " AND active=true",
                p.workspace,
                p.environment,
                service["id"],
                service["version"],
            ):
                raise RouterError("inactive_service_version", 409)
            id = str(uuid.uuid4())
            await tx.execute(
                "INSERT INTO sr_requests(workspace,environment,id,customer,idem,digest,service,legs,deadline,"
                f"route_decision) VALUES($1,$2,$3,$4,$5,$6,$7::jsonb,$8::jsonb,{_MS.format('$9')},$10::jsonb)",
                p.workspace,
                p.environment,
                id,
                p.customer,
                key,
                digest,
                _json(service),
                _json([leg.to_json() for leg in legs]),
                deadline,
                _json(decision),
            )
            for leg in sorted(legs, key=lambda x: x.account):
                remaining = int(leg.reserved)
                buckets = await tx.fetch_all(
                    "SELECT id,remaining::text AS remaining,held::text AS held FROM sr_buckets"
                    " WHERE workspace=$1 AND environment=$2 AND account=$3 AND (expires_at IS NULL OR expires_at>now())"
                    " AND (service_ids IS NULL OR $4=ANY(service_ids))"
                    " ORDER BY expires_at ASC NULLS LAST,"
                    " CASE source_kind WHEN 'subscription' THEN 0 WHEN 'promotion' THEN 1 ELSE 2 END,id FOR UPDATE",
                    p.workspace,
                    p.environment,
                    leg.account,
                    service["id"],
                )
                for b in buckets:
                    take = min(int(b["remaining"]) - int(b["held"]), remaining)
                    if take <= 0:
                        continue
                    await tx.execute(
                        "UPDATE sr_buckets SET held=held+$4::numeric WHERE workspace=$1 AND environment=$2 AND id=$3",
                        p.workspace,
                        p.environment,
                        b["id"],
                        str(take),
                    )
                    await tx.execute(
                        "INSERT INTO sr_allocations(workspace,environment,request,bucket,account,amount)"
                        " VALUES($1,$2,$3,$4,$5,$6::numeric)",
                        p.workspace,
                        p.environment,
                        id,
                        b["id"],
                        leg.account,
                        str(take),
                    )
                    remaining -= take
                    if not remaining:
                        break
                if remaining > 0:
                    raise RouterError("insufficient_balance", 402)
                await tx.execute(
                    "UPDATE sr_accounts SET held=held+$4::numeric WHERE workspace=$1 AND environment=$2 AND id=$3",
                    p.workspace,
                    p.environment,
                    leg.account,
                    leg.reserved,
                )
            return await self._row(tx, p, id), True

    async def begin(self, p: Principal, id: str, offering: Mapping[str, Any], bound: str) -> str:
        async with self._tx(p) as tx:
            await self._check(tx, p, "invoke")
            r = await self._row(tx, p, id)
            if r.cancel_requested or self._clock() > r.deadline:
                raise RouterError("cancelled", 409)
            if r.billing != "reserved":
                raise RouterError("state_conflict", 409)
            if await tx.fetch_one(
                "SELECT 1 AS ok FROM sr_disabled_connections WHERE workspace=$1 AND environment=$2 AND offering=$3",
                p.workspace,
                p.environment,
                offering["id"],
            ):
                raise RouterError("connection_disabled", 409)
            snapshot = next((o for o in r.service["offerings"] if o["id"] == offering["id"]), None)
            if snapshot is None or not _same(snapshot, offering):
                raise RouterError("state_conflict", 409)
            total = await tx.fetch_one(
                "SELECT (provider_spent+provider_held+$4::numeric)::text AS total FROM sr_requests"
                " WHERE workspace=$1 AND environment=$2 AND id=$3",
                p.workspace,
                p.environment,
                id,
                bound,
            )
            assert total is not None
            if int(total["total"]) > int(r.service["max_provider_cost"]):
                raise RouterError("budget_exceeded", 402)
            attempt = str(uuid.uuid4())
            await tx.execute(
                "INSERT INTO sr_attempts(workspace,environment,id,request,offering,cost_bound)"
                " VALUES($1,$2,$3,$4,$5,$6::numeric)",
                p.workspace,
                p.environment,
                attempt,
                id,
                offering["id"],
                bound,
            )
            await tx.execute(
                "UPDATE sr_requests SET execution='running',provider_held=provider_held+$4::numeric"
                " WHERE workspace=$1 AND environment=$2 AND id=$3",
                p.workspace,
                p.environment,
                id,
                bound,
            )
            return attempt

    async def output(self, p: Principal, id: str) -> None:
        async with self._tx(p) as tx:
            await self._row(tx, p, id)
            await tx.execute(
                "UPDATE sr_requests SET output_started=true WHERE workspace=$1 AND environment=$2 AND id=$3",
                p.workspace,
                p.environment,
                id,
            )

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
        async with self._tx(p) as tx:
            r = await self._row(tx, p, id)
            if r.billing not in ("reserved", "pending"):
                raise RouterError("state_conflict", 409)
            row = await tx.fetch_one(
                "SELECT status,cost_bound::text AS bound FROM sr_attempts"
                " WHERE workspace=$1 AND environment=$2 AND id=$3 AND request=$4 FOR UPDATE",
                p.workspace,
                p.environment,
                attempt,
                id,
            )
            if row is None:
                raise RouterError("resource_not_found", 404)
            if row["status"] != "dispatched":
                return
            await tx.execute(
                "UPDATE sr_attempts SET status=$4,usage=$5::jsonb,cost=$6::numeric,upstream_id=$7"
                " WHERE workspace=$1 AND environment=$2 AND id=$3",
                p.workspace,
                p.environment,
                attempt,
                status,
                None if usage is None else _json(usage),
                cost,
                upstream,
            )
            if cost is not None:
                await tx.execute(
                    "UPDATE sr_requests SET provider_spent=provider_spent+$4::numeric,"
                    "provider_held=provider_held-$5::numeric WHERE workspace=$1 AND environment=$2 AND id=$3",
                    p.workspace,
                    p.environment,
                    id,
                    cost,
                    row["bound"],
                )

    async def _event(self, tx: Tx, p: Principal, id: str, kind: str) -> None:
        from datetime import datetime, timezone

        event_id = str(uuid.uuid4())
        occurred = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.%f")[:-3] + "Z"
        payload = {
            "id": event_id,
            "type": kind,
            "schema_version": 1,
            "workspace_id": p.workspace,
            "environment_id": p.environment,
            "request_id": id,
            "occurred_at": occurred,
        }
        await tx.execute(
            "INSERT INTO sr_outbox(id,workspace,environment,request,payload) VALUES($1,$2,$3,$4,$5::jsonb)",
            event_id,
            p.workspace,
            p.environment,
            id,
            _json(payload),
        )

    async def finish(
        self, p: Principal, id: str, execution: str, usage: Mapping[str, Any] | None, pending: bool
    ) -> None:
        async with self._tx(p) as tx:
            await self._finish(tx, p, id, execution, usage, pending)

    async def _finish(
        self, tx: Tx, p: Principal, id: str, execution: str, usage: Mapping[str, Any] | None, pending: bool
    ) -> None:
        r = await self._row(tx, p, id)
        if r.billing in ("settled", "released"):
            return
        where = "workspace=$1 AND environment=$2 AND request=$3"
        if (
            pending
            and not r.output_started
            and not await tx.fetch_one(
                f"SELECT 1 AS ok FROM sr_attempts WHERE {where} LIMIT 1", p.workspace, p.environment, id
            )
        ):
            pending, execution = False, "cancelled"
        if usage is not None and await tx.fetch_one(
            f"SELECT 1 AS ok FROM sr_attempts WHERE {where} AND cost>cost_bound LIMIT 1", p.workspace, p.environment, id
        ):
            pending = True
        due = [charge(usage, leg.price) if usage is not None else 0 for leg in r.legs]
        exceeded = any(d > int(leg.reserved) for d, leg in zip(due, r.legs, strict=True))
        if pending or exceeded:
            if r.billing != "pending":
                await self._event(tx, p, id, "reconciliation.required")
            await tx.execute(
                "UPDATE sr_requests SET execution=$4,billing='pending' WHERE workspace=$1 AND environment=$2 AND id=$3",
                p.workspace,
                p.environment,
                id,
                execution,
            )
            await tx.execute(
                "INSERT INTO sr_reconciliation(workspace,environment,request,reason) VALUES($1,$2,$3,$4)"
                " ON CONFLICT DO NOTHING",
                p.workspace,
                p.environment,
                id,
                "unknown_outcome" if pending else "reservation_exceeded",
            )
            return
        for leg, amount in zip(r.legs, due, strict=True):
            remaining = amount
            allocations = await tx.fetch_all(
                "SELECT a.bucket,a.amount::text AS amount FROM sr_allocations a"
                " JOIN sr_buckets b ON (a.workspace,a.environment,a.bucket)=(b.workspace,b.environment,b.id)"
                " WHERE a.workspace=$1 AND a.environment=$2 AND a.request=$3 AND a.account=$4"
                " ORDER BY b.expires_at ASC NULLS LAST,"
                " CASE b.source_kind WHEN 'subscription' THEN 0 WHEN 'promotion' THEN 1 ELSE 2 END,b.id",
                p.workspace,
                p.environment,
                id,
                leg.account,
            )
            for a in allocations:
                take = min(remaining, int(a["amount"]))
                remaining -= take
                await tx.execute(
                    "UPDATE sr_allocations SET charged=$5::numeric"
                    " WHERE workspace=$1 AND environment=$2 AND request=$3 AND bucket=$4",
                    p.workspace,
                    p.environment,
                    id,
                    a["bucket"],
                    str(take),
                )
                await tx.execute(
                    "UPDATE sr_buckets SET remaining=remaining-$4::numeric,held=held-$5::numeric"
                    " WHERE workspace=$1 AND environment=$2 AND id=$3",
                    p.workspace,
                    p.environment,
                    a["bucket"],
                    str(take),
                    a["amount"],
                )
            await tx.execute(
                "UPDATE sr_accounts SET balance=balance-$4::numeric,held=held-$5::numeric"
                " WHERE workspace=$1 AND environment=$2 AND id=$3",
                p.workspace,
                p.environment,
                leg.account,
                str(amount),
                leg.reserved,
            )
            await tx.execute(
                "INSERT INTO sr_settlements VALUES($1,$2,$3,$4,$5,$6::numeric)",
                p.workspace,
                p.environment,
                id,
                leg.kind,
                leg.account,
                str(amount),
            )
            await tx.execute(
                "INSERT INTO sr_journal(workspace,environment,business_key,asset,debit,credit,amount,reason)"
                " VALUES($1,$2,$3,$4,$5,$6,$7::numeric,$8)",
                p.workspace,
                p.environment,
                f"settle:{id}:{leg.kind}",
                leg.price["asset"],
                leg.account,
                f"consumed:{leg.price['asset']}",
                str(amount),
                "settlement",
            )
        await tx.execute(
            "UPDATE sr_requests SET execution=$4,billing=$5 WHERE workspace=$1 AND environment=$2 AND id=$3",
            p.workspace,
            p.environment,
            id,
            execution,
            "settled" if usage is not None else "released",
        )
        await self._expire(tx, p)
        await self._event(tx, p, id, "billing.settled" if usage is not None else "billing.released")

    async def _expire(self, tx: Tx, p: Principal) -> None:
        """Move the unheld remainder of expired buckets out of the balance."""
        rows = await tx.fetch_all(
            "SELECT b.id,b.account,a.asset,(b.remaining-b.held)::text AS amount FROM sr_buckets b"
            " JOIN sr_accounts a ON (a.workspace,a.environment,a.id)=(b.workspace,b.environment,b.account)"
            " WHERE b.workspace=$1 AND b.environment=$2 AND b.expires_at<=now() AND b.remaining>b.held"
            " ORDER BY b.id FOR UPDATE OF b",
            p.workspace,
            p.environment,
        )
        for b in rows:
            await tx.execute(
                "UPDATE sr_buckets SET remaining=held WHERE workspace=$1 AND environment=$2 AND id=$3",
                p.workspace,
                p.environment,
                b["id"],
            )
            await tx.execute(
                "UPDATE sr_accounts SET balance=balance-$4::numeric WHERE workspace=$1 AND environment=$2 AND id=$3",
                p.workspace,
                p.environment,
                b["account"],
                b["amount"],
            )
            await tx.execute(
                "INSERT INTO sr_journal(workspace,environment,business_key,asset,debit,credit,amount,reason)"
                " VALUES($1,$2,$3,$4,$5,$6,$7::numeric,$8)",
                p.workspace,
                p.environment,
                f"expire:{uuid.uuid4()}",
                b["asset"],
                b["account"],
                f"expired:{b['asset']}",
                b["amount"],
                "expiry",
            )

    async def balance(self, p: Principal) -> list[BalanceRow]:
        async with self._tx(p) as tx:
            await self._expire(tx, p)
            rows = await tx.fetch_all(
                "SELECT id,kind,asset,balance::text AS balance,held::text AS held,(balance-held)::text AS available"
                " FROM sr_accounts WHERE workspace=$1 AND environment=$2"
                " AND (owner=$3 OR (owner=$1 AND kind='platform_sale')) ORDER BY kind,id",
                p.workspace,
                p.environment,
                p.customer,
            )
        return [BalanceRow(r["id"], r["kind"], r["asset"], r["balance"], r["held"], r["available"]) for r in rows]

    async def cancel(self, p: Principal, id: str) -> None:
        async with self._tx(p) as tx:
            await self._row(tx, p, id)
            await tx.execute(
                "UPDATE sr_requests SET cancel_requested=true WHERE workspace=$1 AND environment=$2 AND id=$3",
                p.workspace,
                p.environment,
                id,
            )

    async def disable(self, p: Principal, offering: str) -> None:
        if not p.has_scope("service:write"):
            raise RouterError("scope_denied", 403)
        async with self._tx(p) as tx:
            await tx.execute(
                "INSERT INTO sr_disabled_connections VALUES($1,$2,$3) ON CONFLICT DO NOTHING",
                p.workspace,
                p.environment,
                offering,
            )

    async def recover(self) -> int:
        """Never redispatch an expired uncertain attempt."""
        async with self.db.transaction() as tx:
            targets = await tx.fetch_all(
                "SELECT workspace,environment,customer,id FROM sr_requests"
                " WHERE deadline<now() AND billing='reserved' ORDER BY deadline,id"
            )
        for t in targets:
            p = Principal(t["workspace"], t["environment"], t["customer"])
            async with self._tx(p) as tx:
                r = await self._row(tx, p, t["id"])
                if r.billing != "reserved":
                    continue
                attempts = await tx.fetch_all(
                    "SELECT status,usage FROM sr_attempts WHERE workspace=$1 AND environment=$2 AND request=$3"
                    " ORDER BY created_at,id",
                    p.workspace,
                    p.environment,
                    t["id"],
                )
                last = attempts[-1] if attempts else None
                known = last is not None and last["status"] in ("succeeded", "partial") and last["usage"] is not None
                if known and last is not None:
                    execution, usage = last["status"], _load(last["usage"])
                else:
                    execution, usage = ("unknown" if attempts else "cancelled"), None
                await self._finish(tx, p, t["id"], execution, usage, bool(attempts) and not known)
        return len(targets)

    async def buckets(self, p: Principal) -> list[dict[str, str]]:
        """A tenant's funding buckets ordered by source. For tests and diagnostics."""
        async with self._tx(p) as tx:
            rows = await tx.fetch_all(
                "SELECT source,remaining::text AS remaining,held::text AS held FROM sr_buckets"
                " WHERE workspace=$1 AND environment=$2 ORDER BY source",
                p.workspace,
                p.environment,
            )
        return [dict(r) for r in rows]


class _Scoped:
    """A transaction that first takes the tenant's advisory lock."""

    def __init__(self, db: Database, p: Principal) -> None:
        self._cm = db.transaction()
        self._scope = json.dumps([p.workspace, p.environment], separators=(",", ":"))

    async def __aenter__(self) -> Tx:
        tx = await self._cm.__aenter__()
        try:
            await tx.execute("SELECT pg_advisory_xact_lock(hashtextextended($1,0))", self._scope)
        except BaseException:
            await self._cm.__aexit__(None, None, None)
            raise
        return tx

    async def __aexit__(self, *exc: Any) -> bool | None:
        return await self._cm.__aexit__(*exc)
