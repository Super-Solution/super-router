# SPDX-License-Identifier: Apache-2.0
"""Server-sent events across arbitrary chunk and CRLF boundaries. Content is never logged."""

from __future__ import annotations

from collections.abc import AsyncIterator
from dataclasses import dataclass

MAX_FRAME_BYTES = 2_000_000  # one event, so a hostile upstream cannot exhaust memory


class FrameTooLarge(Exception):
    pass


@dataclass
class Frame:
    event: str
    data: str


async def read_sse(chunks: AsyncIterator[bytes]) -> AsyncIterator[Frame]:
    """Yield each complete event in order. A trailing event with no terminating blank line is dropped, as in the
    TypeScript reference and the Go port."""
    buffer = bytearray()
    event, size = "message", 0
    data: list[str] = []
    async for chunk in chunks:
        buffer += chunk
        while (end := buffer.find(b"\n")) >= 0:
            raw = bytes(buffer[: end + 1])
            del buffer[: end + 1]
            size += len(raw)
            if size > MAX_FRAME_BYTES:
                raise FrameTooLarge
            line = raw.decode("utf-8", "replace").rstrip("\r\n")
            if line == "":
                if data:
                    yield Frame(event, "\n".join(data))
                event, size, data = "message", 0, []
            elif line.startswith("event:"):
                event = line[len("event:") :].strip()
            elif line.startswith("data:"):
                value = line[len("data:") :]
                data.append(value[1:] if value.startswith(" ") else value)
        if size + len(buffer) > MAX_FRAME_BYTES:
            raise FrameTooLarge
