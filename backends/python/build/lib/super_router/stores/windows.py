# SPDX-License-Identifier: Apache-2.0
"""Optional session + weekly usage windows on subscription credits (docs/WINDOW_LIMITS.md).

A window opens at the first reservation drawn from a subscription's bucket after the previous window ended and
closes after its length. The limits shape how fast the period's credits can be spent; they never add credits.
Everything here runs inside the tenant transaction of the reservation, so concurrent reservations of one tenant
cannot overshoot a limit.
"""

from __future__ import annotations

import json
import uuid
from dataclasses import dataclass
from datetime import datetime, timezone
from typing import Any

from ..contracts import Principal
from ..pricing import RouterError
from .sql import Tx

WEEK_HOURS = 168  # the long window is fixed; only the session window's length is configurable


@dataclass(frozen=True)
class WindowSpec:
    kind: str
    hours: int
    limit: int


@dataclass(frozen=True)
class WindowBlock:
    """A window that held a draw below what was wanted."""

    kind: str
    limit: int
    used: int
    resets_ms: int | None  # when the open window closes; None when none is open (the draw alone exceeds the limit)
    retryable: bool  # False when even an empty window could not admit the draw, so waiting does not help


def window_specs(raw: str | None) -> list[WindowSpec]:
    """The enforced windows of an entitlement snapshot; empty when the option is off or absent."""
    if not raw:
        return []
    w = json.loads(raw)
    if not w or not w.get("enabled"):
        return []
    out: list[WindowSpec] = []
    if w.get("session"):
        out.append(WindowSpec("session", int(w["session"]["hours"]), int(w["session"]["credits"])))
    if w.get("weekly"):
        out.append(WindowSpec("weekly", WEEK_HOURS, int(w["weekly"]["credits"])))
    return out


def _worse(a: WindowBlock, b: WindowBlock | None) -> bool:
    """Which of two blocking windows to report: one that waiting cannot fix, else the one that resets last."""
    if b is None:
        return True
    if not a.retryable and b.retryable:
        return True
    if a.retryable != b.retryable:
        return False
    return (a.resets_ms or 0) > (b.resets_ms or 0)


def _iso(ms: int | None) -> str | None:
    if ms is None:
        return None
    t = datetime.fromtimestamp(ms / 1000, tz=timezone.utc)
    return t.strftime("%Y-%m-%dT%H:%M:%S.") + f"{ms % 1000:03d}Z"


async def admit(
    tx: Tx, p: Principal, subscription: str, specs: list[WindowSpec], want: int
) -> tuple[int, list[str], WindowBlock | None]:
    """Admit up to ``want`` of the subscription's credits against every enforced window.

    Returns how much may be drawn (the smallest headroom), the windows that now hold it, and, when a window held the
    draw below ``want``, the one to report. Windows open lazily and only when something is actually drawn.
    """
    rows: list[Any] = []
    take = want
    blocked: WindowBlock | None = None
    for spec in specs:
        row = await tx.fetch_one(
            "SELECT id,(extract(epoch from window_end)*1000)::bigint AS end_ms,spent::text AS spent,held::text AS held"
            " FROM sr_usage_windows WHERE workspace=$1 AND environment=$2 AND subscription=$3 AND kind=$4"
            " AND window_end>now() ORDER BY window_start DESC LIMIT 1 FOR UPDATE",
            p.workspace,
            p.environment,
            subscription,
            spec.kind,
        )
        rows.append(row)
        used = int(row["spent"]) + int(row["held"]) if row else 0
        room = max(spec.limit - used, 0)
        if room < want:
            block = WindowBlock(
                spec.kind, spec.limit, used, int(row["end_ms"]) if row else None, retryable=want <= spec.limit
            )
            if _worse(block, blocked):
                blocked = block
            take = min(take, room)
    if take <= 0:
        return 0, [], blocked
    ids: list[str] = []
    for spec, row in zip(specs, rows, strict=True):
        if row is None:
            window_id = str(uuid.uuid4())
            await tx.execute(
                "INSERT INTO sr_usage_windows(workspace,environment,id,subscription,kind,window_start,window_end)"
                " VALUES($1,$2,$3,$4,$5,now(),now()+make_interval(hours=>$6))",
                p.workspace,
                p.environment,
                window_id,
                subscription,
                spec.kind,
                spec.hours,
            )
        else:
            window_id = str(row["id"])
        await tx.execute(
            "UPDATE sr_usage_windows SET held=held+$4::numeric WHERE workspace=$1 AND environment=$2 AND id=$3",
            p.workspace,
            p.environment,
            window_id,
            str(take),
        )
        ids.append(window_id)
    return take, ids, blocked


async def settle(tx: Tx, p: Principal, ids: list[str], held: str, charged: str) -> None:
    """An allocation settled: its hold leaves the windows and what was actually charged enters them.

    A window left with nothing spent and nothing held (every request in it was released) is dropped, so a request
    that cost nothing does not start anyone's session clock.
    """
    if not ids:
        return
    listing = json.dumps(ids)
    await tx.execute(
        "UPDATE sr_usage_windows SET held=held-$4::numeric,spent=spent+$5::numeric"
        " WHERE workspace=$1 AND environment=$2 AND id=ANY(SELECT jsonb_array_elements_text($3::jsonb))",
        p.workspace,
        p.environment,
        listing,
        held,
        charged,
    )
    await tx.execute(
        "DELETE FROM sr_usage_windows WHERE workspace=$1 AND environment=$2"
        " AND id=ANY(SELECT jsonb_array_elements_text($3::jsonb)) AND spent=0 AND held=0",
        p.workspace,
        p.environment,
        listing,
    )


def window_error(b: WindowBlock) -> RouterError:
    """The customer-facing refusal: which window, how full, when it resets."""
    return RouterError(
        "window_limit_reached",
        429,
        b.retryable,
        {"window": b.kind, "limit": str(b.limit), "used": str(b.used), "resets_at": _iso(b.resets_ms)},
    )


async def usage(tx: Tx, p: Principal) -> list[dict[str, Any]]:
    """The customer's window usage for subscriptions with the option on.

    The limits are those of each subscription's current entitlement. A window that has not opened yet reports
    nothing spent and no reset time: it starts with the next request.
    """
    entitlements = await tx.fetch_all(
        "SELECT DISTINCT ON (subscription) subscription,window_limits::text AS window_limits FROM sr_entitlements"
        " WHERE workspace=$1 AND environment=$2 AND customer=$3 AND period_start<=now() AND period_end>now()"
        " ORDER BY subscription,cycle DESC",
        p.workspace,
        p.environment,
        p.customer,
    )
    out: list[dict[str, Any]] = []
    for e in entitlements:
        for spec in window_specs(e["window_limits"]):
            row = await tx.fetch_one(
                "SELECT (extract(epoch from window_start)*1000)::bigint AS start_ms,"
                " (extract(epoch from window_end)*1000)::bigint AS end_ms,spent::text AS spent,held::text AS held"
                " FROM sr_usage_windows WHERE workspace=$1 AND environment=$2 AND subscription=$3 AND kind=$4"
                " AND window_end>now() ORDER BY window_start DESC LIMIT 1",
                p.workspace,
                p.environment,
                e["subscription"],
                spec.kind,
            )
            used = int(row["spent"]) + int(row["held"]) if row else 0
            out.append(
                {
                    "subscription": e["subscription"],
                    "window": spec.kind,
                    "hours": spec.hours,
                    "limit": str(spec.limit),
                    "spent": row["spent"] if row else "0",
                    "held": row["held"] if row else "0",
                    "remaining": str(max(spec.limit - used, 0)),
                    "window_start": _iso(int(row["start_ms"])) if row else None,
                    "resets_at": _iso(int(row["end_ms"])) if row else None,
                }
            )
    return out
