"""Stable application entrypoint for the unified Cool package."""

from __future__ import annotations

import argparse
import json
import os
from collections.abc import Sequence
from dataclasses import asdict
from pathlib import Path


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

    commands.add_parser("acp", help="serve Agent Client Protocol v1 over stdio")
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

    plugin = commands.add_parser("plugin", help="validate, inspect and manage plugin bundles")
    plugin_commands = plugin.add_subparsers(dest="plugin_command", required=True)
    validate = plugin_commands.add_parser("validate", help="validate Agent Plugins conformance")
    validate.add_argument("path", type=Path)
    doctor = plugin_commands.add_parser(
        "doctor", help="show components and compatibility diagnostics"
    )
    doctor.add_argument("target", help="bundle path or installed plugin name")
    install = plugin_commands.add_parser("install", help="install a local or pinned Git bundle")
    install.add_argument("source")
    install.add_argument("--git", action="store_true", help="treat source as a Git repository")
    install.add_argument("--revision", help="required full commit SHA for --git")
    update = plugin_commands.add_parser("update", help="replace an installed bundle")
    update.add_argument("name")
    update.add_argument("source")
    update.add_argument("--git", action="store_true", help="treat source as a Git repository")
    update.add_argument("--revision", help="required full commit SHA for --git")
    plugin_commands.add_parser("list", help="list installed plugins")
    for action in ("enable", "disable"):
        command = plugin_commands.add_parser(action, help=f"{action} an installed plugin")
        command.add_argument("name")
    remove = plugin_commands.add_parser("remove", help="remove an installed plugin")
    remove.add_argument("name")
    remove.add_argument(
        "--purge-data",
        action="store_true",
        help="also permanently delete the plugin's mutable data root",
    )
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    if args.command == "acp":
        import asyncio
        import sys

        from app.acp import run_stdio
        from app.core.db import init_db
        from app.core.logging import configure_logging

        # stdout is reserved exclusively for JSON-RPC frames.
        configure_logging(stream=sys.stderr)
        init_db()
        asyncio.run(run_stdio())
        return 0

    if args.command == "plugin":
        from app.plugins import (
            CompatibilityLoader,
            PluginLoader,
            PluginStore,
            PluginStoreError,
        )

        try:
            if args.plugin_command == "validate":
                bundle = PluginLoader().load(args.path)
                report, code = bundle.to_dict(), 0 if bundle.conformant else 1
            elif args.plugin_command == "doctor":
                candidate = Path(args.target)
                if candidate.exists():
                    bundle = CompatibilityLoader().load(candidate)
                    report, code = bundle.doctor_dict(), 0 if bundle.manifest is not None else 1
                else:
                    report, code = PluginStore().doctor(args.target), 0
            else:
                store = PluginStore()
                if args.plugin_command == "list":
                    report, code = {"plugins": [asdict(item) for item in store.list_installed()]}, 0
                elif args.plugin_command == "install":
                    if args.git and not args.revision:
                        raise PluginStoreError("--revision is required with --git")
                    if not args.git and args.revision:
                        raise PluginStoreError("--revision is valid only with --git")
                    entry = (
                        store.install_git(args.source, args.revision)
                        if args.git
                        else store.install_local(Path(args.source))
                    )
                    report, code = asdict(entry), 0
                elif args.plugin_command == "update":
                    if args.git and not args.revision:
                        raise PluginStoreError("--revision is required with --git")
                    if not args.git and args.revision:
                        raise PluginStoreError("--revision is valid only with --git")
                    entry = (
                        store.update_git(args.name, args.source, args.revision)
                        if args.git
                        else store.update_local(args.name, Path(args.source))
                    )
                    report, code = asdict(entry), 0
                elif args.plugin_command in {"enable", "disable"}:
                    entry = store.set_enabled(args.name, args.plugin_command == "enable")
                    report, code = asdict(entry), 0
                elif args.plugin_command == "remove":
                    entry = store.remove(args.name, purge_data=args.purge_data)
                    report, code = asdict(entry), 0
                else:  # pragma: no cover - argparse enforces the choices
                    raise AssertionError(args.plugin_command)
        except PluginStoreError as exc:
            report, code = {"error": str(exc)}, 1
        print(json.dumps(report, indent=2, ensure_ascii=False, sort_keys=True))
        return code

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
