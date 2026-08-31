"""Stable application entrypoint for the unified Cool package."""

from __future__ import annotations

import argparse
import os
from collections.abc import Sequence


def _port(value: str) -> int:
    port = int(value)
    if not 1 <= port <= 65_535:
        raise argparse.ArgumentTypeError("port must be between 1 and 65535")
    return port


def _env_flag(name: str, *, default: bool = False) -> bool:
    value = os.environ.get(name)
    if value is None:
        return default
    return value.strip().lower() in {"1", "true", "yes", "on"}


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="cool",
        description="Cool AI Harness unified application entrypoint",
    )
    parser.add_argument("--version", action="version", version="%(prog)s 0.1.0")
    commands = parser.add_subparsers(dest="command", required=True)

    serve = commands.add_parser("serve", help="serve the SPA, API and WebSocket on one port")
    serve.add_argument("--host", default=os.environ.get("COOL_HOST", "127.0.0.1"))
    serve.add_argument(
        "--port",
        type=_port,
        default=_port(os.environ.get("COOL_PORT", "8000")),
    )
    serve.add_argument(
        "--log-level",
        choices=("critical", "error", "warning", "info", "debug", "trace"),
        default=os.environ.get("COOL_LOG_LEVEL", "info"),
    )
    serve.add_argument(
        "--proxy-headers",
        action=argparse.BooleanOptionalAction,
        default=_env_flag("COOL_PROXY_HEADERS"),
        help="trust proxy forwarding headers (off by default)",
    )
    serve.add_argument(
        "--forwarded-allow-ips",
        default=os.environ.get("COOL_FORWARDED_ALLOW_IPS", "127.0.0.1"),
        help="trusted proxy IPs; only used with --proxy-headers",
    )
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    if args.command != "serve":  # pragma: no cover - argparse enforces this
        raise AssertionError(f"unsupported command: {args.command}")

    import uvicorn

    uvicorn.run(
        "app.main:app",
        host=args.host,
        port=args.port,
        log_level=args.log_level,
        proxy_headers=args.proxy_headers,
        forwarded_allow_ips=(args.forwarded_allow_ips if args.proxy_headers else ""),
        workers=1,
    )
    return 0


if __name__ == "__main__":  # pragma: no cover
    raise SystemExit(main())
