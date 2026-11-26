# SPDX-License-Identifier: Apache-2.0
"""The minimal database port the SQL stores need, plus a psycopg 3 adapter.

Write SQL with numbered placeholders (``$1``, ``$2``, ... as PostgreSQL itself does) and pass the values as
arguments. To use another driver or an ORM's connection pool, implement :class:`Database` (about thirty lines):
open a transaction, run statements, return rows as dicts. asyncpg already uses ``$n`` natively; SQLAlchemy can
run the same text through ``exec_driver_sql`` after rewriting ``$n`` to ``:pn``.
"""

from __future__ import annotations

import re
from collections.abc import AsyncIterator, Mapping
from contextlib import AbstractAsyncContextManager, asynccontextmanager
from typing import Any, Protocol

Row = Mapping[str, Any]


class Tx(Protocol):
    async def execute(self, sql: str, *params: Any) -> None: ...

    async def fetch_one(self, sql: str, *params: Any) -> Row | None: ...

    async def fetch_all(self, sql: str, *params: Any) -> list[Row]: ...


class Database(Protocol):
    def transaction(self) -> AbstractAsyncContextManager[Tx]:
        """Begin a transaction: commit when the block ends, roll back when it raises."""
        ...


_PLACEHOLDER = re.compile(r"\$(\d+)")


def _psycopg_query(sql: str, params: tuple[Any, ...]) -> tuple[str, dict[str, Any]]:
    """Rewrite ``$n`` to psycopg's named placeholders, so a parameter can be used several times."""
    return _PLACEHOLDER.sub(r"%(\1)s", sql.replace("%", "%%")), {str(i + 1): v for i, v in enumerate(params)}


class _PsycopgTx:
    def __init__(self, conn: Any) -> None:
        self._conn = conn

    async def execute(self, sql: str, *params: Any) -> None:
        query, args = _psycopg_query(sql, params)
        await self._conn.execute(query, args)

    async def fetch_one(self, sql: str, *params: Any) -> Row | None:
        query, args = _psycopg_query(sql, params)
        cursor = await self._conn.execute(query, args)
        row: Row | None = await cursor.fetchone()
        return row

    async def fetch_all(self, sql: str, *params: Any) -> list[Row]:
        query, args = _psycopg_query(sql, params)
        cursor = await self._conn.execute(query, args)
        rows: list[Row] = await cursor.fetchall()
        return rows


class PsycopgDatabase:
    """:class:`Database` on a psycopg 3 ``AsyncConnectionPool`` (``pip install super-router[postgres]``).

    The pool must be created with ``kwargs={"row_factory": psycopg.rows.dict_row}``; ``connect`` does this for you.
    """

    def __init__(self, pool: Any) -> None:
        self.pool = pool

    @classmethod
    async def connect(cls, url: str, max_size: int = 10) -> PsycopgDatabase:
        from psycopg.rows import dict_row
        from psycopg_pool import AsyncConnectionPool

        pool = AsyncConnectionPool(url, min_size=1, max_size=max_size, open=False, kwargs={"row_factory": dict_row})
        await pool.open(wait=True)
        return cls(pool)

    async def close(self) -> None:
        await self.pool.close()

    @asynccontextmanager
    async def transaction(self) -> AsyncIterator[Tx]:
        async with self.pool.connection() as conn, conn.transaction():
            yield _PsycopgTx(conn)
