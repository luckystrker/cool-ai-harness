"""Newline-delimited JSON-RPC transport for ``cool acp``."""

from __future__ import annotations

import asyncio
import json
import sys
from typing import Any

from app.acp.server import ACPConnection

MAX_MESSAGE_BYTES = 1_048_576
MAX_JSON_DEPTH = 128


def _reject_non_finite(value: str) -> None:
    raise ValueError(f"non-finite JSON number is not allowed: {value}")


def decode_message(raw: bytes) -> Any:
    """Decode strict JSON without non-standard numbers or recursion crashes."""
    depth = 0
    in_string = False
    escaped = False
    for byte in raw:
        if in_string:
            if escaped:
                escaped = False
            elif byte == 0x5C:  # backslash
                escaped = True
            elif byte == 0x22:  # quote
                in_string = False
            continue
        if byte == 0x22:
            in_string = True
        elif byte in (0x5B, 0x7B):  # [ or {
            depth += 1
            if depth > MAX_JSON_DEPTH:
                raise ValueError("JSON nesting limit exceeded")
        elif byte in (0x5D, 0x7D):  # ] or }
            depth -= 1
    return json.loads(raw, parse_constant=_reject_non_finite)


async def run_stdio() -> None:
    """Run one ACP connection over process stdin/stdout until EOF."""
    write_lock = asyncio.Lock()

    async def send(message: Any) -> None:
        encoded = json.dumps(message, default=str, ensure_ascii=False, separators=(",", ":"))
        async with write_lock:
            sys.stdout.write(encoded + "\n")
            sys.stdout.flush()

    connection = ACPConnection(send)
    try:
        while True:
            raw = await asyncio.to_thread(sys.stdin.buffer.readline, MAX_MESSAGE_BYTES + 1)
            if not raw:
                break
            if len(raw) > MAX_MESSAGE_BYTES:
                await send(
                    {
                        "jsonrpc": "2.0",
                        "id": None,
                        "error": {"code": -32700, "message": "message exceeds size limit"},
                    }
                )
                if not raw.endswith(b"\n"):
                    while True:
                        tail = await asyncio.to_thread(
                            sys.stdin.buffer.readline, MAX_MESSAGE_BYTES + 1
                        )
                        if not tail or tail.endswith(b"\n"):
                            break
                continue
            try:
                message = decode_message(raw)
            except (UnicodeDecodeError, ValueError, RecursionError):
                await send(
                    {
                        "jsonrpc": "2.0",
                        "id": None,
                        "error": {"code": -32700, "message": "parse error"},
                    }
                )
                continue
            await connection.receive(message)
    finally:
        await connection.close()


__all__ = ["MAX_JSON_DEPTH", "MAX_MESSAGE_BYTES", "decode_message", "run_stdio"]
