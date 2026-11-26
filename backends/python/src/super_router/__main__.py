# SPDX-License-Identifier: Apache-2.0
"""``python -m super_router`` (or ``super-router-server``): the data plane with a storage backend and providers.

Configured through environment variables, as the Go binary is:

    SUPERROUTER_API_KEY      "sr_" plus 64 hexadecimal characters; the bearer token of the single owner key.
                             Required unless SUPERROUTER_AUTH=database
    SUPERROUTER_DIGEST_KEY   required, at least 32 characters; keys the idempotency digest
    SUPERROUTER_AUTH         "database" authenticates bearer tokens against the control plane's keys in PostgreSQL
                             (members, scoped keys, revocation) instead of the single owner key; needs DATABASE_URL.
                             Keys are created by the TypeScript control plane
    DATABASE_URL             PostgreSQL URL. Unset means the in-memory store: state is lost on restart
    ADDR                     listen address, default 127.0.0.1:3410
    SUPERROUTER_WORKSPACE, SUPERROUTER_ENVIRONMENT, SUPERROUTER_CUSTOMER
                             tenant of the owner key, default default, prod, default
    OPENAI_API_KEY, ANTHROPIC_API_KEY
                             enable the "openai" and "anthropic" providers (offering.provider names them)
    OPENAI_BASE_URL, ANTHROPIC_BASE_URL
                             where that provider's requests go: the scheme, host and any path prefix, without the
                             trailing /v1 the provider adds itself, so https://llm.internal/openai becomes
                             https://llm.internal/openai/v1/responses. Default: the vendor's own host
    SUPERROUTER_HEALTH       "off" or unset. "on" (and the SUPERROUTER_HEALTH_*_MS policy variables the Go and Rust
                             servers read) is refused: this server has no health book yet
    SUPERROUTER_MOCK=1       enable the deterministic "mock" provider

Needs ``pip install super-router[server]`` (uvicorn, httpx, psycopg).
"""

from __future__ import annotations

import asyncio
import contextlib
import logging
import re
import signal
import sys
from collections.abc import Callable, Iterator, Mapping
from typing import Any

from .contracts import Principal, Provider
from .router import DataStore, MockProvider, Router
from .server import Authenticator, Key, StaticKeys, create_app
from .stores.memory import MemoryStore

_API_KEY = re.compile(r"^sr_[a-f0-9]{64}$")
_log = logging.getLogger("super_router")


class ConfigError(Exception):
    pass


def _base_url(name: str, value: str) -> str:
    """A provider base URL: http or https, a host, an optional path prefix and nothing else. The version segment is the
    provider's own (/v1/responses), so a value that already ends in one is refused instead of producing /v1/v1/...
    The value is never echoed: a URL can carry credentials."""

    def bad(why: str) -> ConfigError:
        return ConfigError(
            f"{name} {why} (the scheme, host and any path prefix, without the trailing /v1; value not logged)"
        )

    rest = value.removeprefix("https://") if value.startswith("https://") else value.removeprefix("http://")
    if rest == value:
        raise bad("must start with http:// or https://")
    if any(ord(c) < 0x20 or c in " ?#\x7f" for c in rest):
        raise bad("must have no spaces, query or fragment")
    authority, _, path = rest.partition("/")
    if not authority or "@" in authority:
        raise bad("must name a host and carry no credentials")
    if path.rstrip("/").rpartition("/")[2] in ("v1", "v1beta"):
        raise bad("must not end in the version segment, the provider adds it")
    return value.rstrip("/")


async def _recover_loop(store: DataStore, every: float) -> None:
    """Settle or release requests whose deadline passed, for example after a crash mid-call."""
    while True:
        await asyncio.sleep(every)
        try:
            if (count := await store.recover()) > 0:
                _log.info("recovered expired requests count=%d", count)
        except Exception:
            _log.exception("recover failed")


async def serve(
    getenv: Callable[[str], str | None],
    ready: asyncio.Future[tuple[str, int]] | None = None,
    stop: asyncio.Event | None = None,
    recover_every: float = 30.0,
) -> None:
    """Run until ``stop`` is set (or the process gets SIGINT/SIGTERM). ``ready`` receives the bound (host, port)."""
    import uvicorn

    def env(name: str, fallback: str = "") -> str:
        return getenv(name) or fallback

    api_key, digest_key = env("SUPERROUTER_API_KEY"), env("SUPERROUTER_DIGEST_KEY")
    auth_mode = env("SUPERROUTER_AUTH")
    if auth_mode not in ("", "database"):
        raise ConfigError('SUPERROUTER_AUTH must be empty or "database"')
    database_auth = auth_mode == "database"
    if database_auth and not env("DATABASE_URL"):
        raise ConfigError("SUPERROUTER_AUTH=database needs DATABASE_URL")
    if (not database_auth and not _API_KEY.match(api_key)) or len(digest_key) < 32:
        raise ConfigError(
            "set SUPERROUTER_API_KEY (sr_ plus 64 random hexadecimal characters; not needed with "
            "SUPERROUTER_AUTH=database) and SUPERROUTER_DIGEST_KEY (at least 32 characters); values are not logged"
        )
    host, _, port = env("ADDR", "127.0.0.1:3410").rpartition(":")
    if not host or not (port.isascii() and port.isdigit()):
        raise ConfigError("ADDR must be host:port")

    health = env("SUPERROUTER_HEALTH")
    if health not in ("", "off", "on"):
        raise ConfigError('SUPERROUTER_HEALTH must be "on" or "off"')
    if health == "on":
        raise ConfigError("SUPERROUTER_HEALTH=on: the Python server has no health book yet, use the Go or Rust server")
    bases = {name: _base_url(name, env(name)) for name in ("OPENAI_BASE_URL", "ANTHROPIC_BASE_URL") if env(name)}

    closers: list[Callable[[], Any]] = []
    store: DataStore
    auth: Authenticator | None = None
    if url := env("DATABASE_URL"):
        from .stores.keys import PostgresKeys, revalidate
        from .stores.postgres import PostgresStore
        from .stores.sql import PsycopgDatabase

        db = await PsycopgDatabase.connect(url, max_size=20)
        closers.append(db.close)
        pg = PostgresStore(db, revalidate=revalidate if database_auth else None)
        await pg.migrate()
        store = pg
        if database_auth:
            auth = PostgresKeys(db)
    else:
        _log.warning("DATABASE_URL is not set: using the in-memory store, state is lost on restart")
        store = MemoryStore()

    providers: dict[str, Provider] = {}
    if key := env("OPENAI_API_KEY"):
        from .providers import OpenAIProvider

        openai = OpenAIProvider(key, bases.get("OPENAI_BASE_URL", "https://api.openai.com"))
        closers.append(openai.aclose)
        providers["openai"] = openai
    if key := env("ANTHROPIC_API_KEY"):
        from .providers import AnthropicProvider

        anthropic = AnthropicProvider(key, bases.get("ANTHROPIC_BASE_URL", "https://api.anthropic.com"))
        closers.append(anthropic.aclose)
        providers["anthropic"] = anthropic
    if env("SUPERROUTER_MOCK") == "1":
        providers["mock"] = MockProvider("success")
    if not providers:
        _log.warning("no provider is configured: set OPENAI_API_KEY, ANTHROPIC_API_KEY or SUPERROUTER_MOCK=1")

    owner = Principal(
        env("SUPERROUTER_WORKSPACE", "default"),
        env("SUPERROUTER_ENVIRONMENT", "prod"),
        env("SUPERROUTER_CUSTOMER", "default"),
        ("invoke", "service:write", "billing:write", "billing:read", "operations:read", "routing:write"),
    )
    router = Router(store, providers, digest_key)
    app = create_app(router, store, auth or StaticKeys([Key(api_key, owner)]), logger=_log)

    class Server(uvicorn.Server):
        """We own signal handling, so shutdown ends in our cleanup and a normal exit, not a re-raised signal."""

        def install_signal_handlers(self) -> None:
            pass

        @contextlib.contextmanager
        def capture_signals(self) -> Iterator[None]:
            yield

    server = Server(uvicorn.Config(app, host=host, port=int(port), log_level="warning", access_log=False))
    if stop is None:
        stop = asyncio.Event()
        loop = asyncio.get_running_loop()
        for name in ("SIGINT", "SIGTERM"):
            with contextlib.suppress(NotImplementedError, ValueError, AttributeError):
                loop.add_signal_handler(getattr(signal, name), stop.set)
    recover = asyncio.create_task(_recover_loop(store, recover_every))
    serving = asyncio.create_task(server.serve())
    try:
        while not server.started and not serving.done():
            await asyncio.sleep(0.01)
        if server.started:
            bound = server.servers[0].sockets[0].getsockname()
            _log.info(
                "listening addr=%s:%d store=%s providers=%d", bound[0], bound[1], type(store).__name__, len(providers)
            )
            if ready is not None and not ready.done():
                ready.set_result((bound[0], bound[1]))
        waiter = asyncio.create_task(stop.wait())
        await asyncio.wait({serving, waiter}, return_when=asyncio.FIRST_COMPLETED)
        server.should_exit = True
        await serving
    finally:
        recover.cancel()
        await asyncio.gather(recover, return_exceptions=True)
        for close in closers:
            with contextlib.suppress(Exception):
                await close()


def main(argv: list[str] | None = None, environ: Mapping[str, str] | None = None) -> int:
    import os

    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(name)s %(message)s", stream=sys.stderr)
    source = os.environ if environ is None else environ
    try:
        asyncio.run(serve(source.get))
    except ConfigError as e:
        _log.error("%s", e)
        return 1
    except KeyboardInterrupt:
        pass
    return 0


if __name__ == "__main__":
    sys.exit(main())
