# SPDX-License-Identifier: Apache-2.0
"""Database-backed API keys: the control plane's ``sr_keys`` and ``sr_members`` tables, written by the TypeScript
control plane (members, scoped keys, rotation, revocation) and read here.

``PostgresKeys`` is a server ``Authenticator``; ``revalidate`` goes to ``PostgresStore(revalidate=...)`` so a write
is refused if its key was revoked or narrowed after the request was authenticated."""

from __future__ import annotations

import hashlib
import json
import re

from ..contracts import ROLE_SCOPES, Principal
from ..pricing import RouterError
from .sql import Database, Tx

_TOKEN = re.compile(r"^sr_[a-f0-9]{64}$")


def hash_token(token: str) -> str:
    return hashlib.sha256(token.encode()).hexdigest()


async def _principal_by_hash(tx: Tx, digest: str) -> Principal:
    row = await tx.fetch_one(
        "SELECT k.workspace,k.environment,k.customer,k.id,k.member_id,to_json(k.scopes)::text AS scopes,"
        "to_json(k.service_ids)::text AS service_ids,m.role,m.active"
        " FROM sr_keys k JOIN sr_members m ON (k.workspace,k.environment,k.member_id)=(m.workspace,m.environment,m.id)"
        " WHERE k.hash=$1 AND k.revoked_at IS NULL AND k.expires_at>now()",
        digest,
    )
    if row is None or not row["active"]:
        raise RouterError("invalid_key", 401)
    allowed = ROLE_SCOPES.get(row["role"], ())
    services = json.loads(row["service_ids"]) if row["service_ids"] else None
    return Principal(
        row["workspace"],
        row["environment"],
        row["customer"],
        tuple(s for s in json.loads(row["scopes"]) if s in allowed),
        auth_key_id=row["id"],
        member_id=row["member_id"],
        service_ids=tuple(services) if services is not None else None,
    )


class PostgresKeys:
    """Authenticate bearer tokens against the key table. Only the SHA-256 of a token is stored; revoked, expired and
    deactivated-member keys are refused, and scopes are narrowed to what the member's role still allows."""

    def __init__(self, db: Database) -> None:
        self._db = db

    async def authenticate(self, token: str) -> Principal:
        if not _TOKEN.match(token):
            raise RouterError("invalid_key", 401)
        async with self._db.transaction() as tx:
            return await _principal_by_hash(tx, hash_token(token))


async def revalidate(tx: Tx, p: Principal, _scope: str) -> None:
    """Re-read the principal's key inside a write transaction. Principals that did not come from a key pass."""
    if p.auth_key_id is None:
        return
    row = await tx.fetch_one(
        "SELECT hash FROM sr_keys WHERE workspace=$1 AND environment=$2 AND id=$3",
        p.workspace,
        p.environment,
        p.auth_key_id,
    )
    if row is None:
        raise RouterError("invalid_key", 401)
    current = await _principal_by_hash(tx, row["hash"])
    if current.customer != p.customer or any(s not in current.scopes for s in p.scopes):
        raise RouterError("scope_denied", 403)
    if current.service_ids is not None and (
        p.service_ids is None or any(s not in current.service_ids for s in p.service_ids)
    ):
        raise RouterError("scope_denied", 403)
