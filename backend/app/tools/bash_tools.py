"""Sandboxed shell command execution tool.

Runs shell commands in a subprocess with timeouts and output limits. Uses
``cmd.exe /c`` on Windows and ``/bin/bash -c`` on Unix. Host secrets are
stripped from the subprocess environment via ``build_sandbox_env()``.

This is NOT a full security sandbox (no filesystem isolation, no cgroups).
The MVP is single-user and trusted; do not expose to untrusted multi-tenant
traffic.
"""

from __future__ import annotations

import asyncio
import subprocess
import sys
from pathlib import Path

from app.core.config import get_settings
from app.security.capabilities import Capability
from app.security.sandbox import build_sandbox_env
from app.security.secrets import mask_tool_output
from app.tools.base import ToolArgs, ToolResult, register_tool
from app.tools.context import get_run_context
from app.tools.subprocess_cancellation import (
    communicate_cancellable,
    kill_and_reap,
    process_group_kwargs,
    run_in_thread_cancellable,
)


class BashExecuteArgs(ToolArgs):
    command: str
    timeout: float = 30.0
    max_output_chars: int = 20_000


def _shell_argv(command: str) -> list[str]:
    """Build the argv for running a shell command cross-platform."""
    if sys.platform == "win32":
        # Prefer cmd.exe for simple commands; it's always available.
        return ["cmd.exe", "/c", command]
    return ["/bin/bash", "-c", command]


def _loop_supports_subprocess(loop: asyncio.AbstractEventLoop) -> bool:
    """True if the running loop can spawn subprocesses via the asyncio API."""
    if sys.platform != "win32":
        return True
    return type(loop).__name__ == "ProactorEventLoop"


async def bash_execute(
    *, command: str, timeout: float = 30.0, max_output_chars: int = 20_000
) -> ToolResult:
    """Run a shell command in a subprocess; return captured stdout/stderr."""
    argv = _shell_argv(command)
    workdir = get_run_context().workdir

    settings = get_settings()
    sandbox_env = build_sandbox_env(strip_secrets=settings.sandbox_strip_env)

    loop = asyncio.get_running_loop()
    if _loop_supports_subprocess(loop):
        result = await _run_async(argv, timeout, cwd=workdir, env=sandbox_env)
    else:
        result = await run_in_thread_cancellable(
            _run_sync, argv, timeout, cwd=workdir, env=sandbox_env
        )

    if isinstance(result, str):
        return ToolResult.err(result)

    stdout_b, stderr_b, returncode = result
    stdout = stdout_b.decode("utf-8", errors="replace") if stdout_b else ""
    stderr = stderr_b.decode("utf-8", errors="replace") if stderr_b else ""

    truncated = False
    if len(stdout) > max_output_chars:
        stdout = stdout[:max_output_chars] + f"\n[... truncated at {max_output_chars} chars]"
        truncated = True

    safe_stdout = mask_tool_output(stdout.strip() or "(no stdout)")
    safe_stderr = mask_tool_output(stderr.strip()) if stderr.strip() else None
    return ToolResult.ok(
        safe_stdout,
        stderr=safe_stderr,
        exit_code=returncode,
        truncated=truncated,
    )


async def _run_async(
    argv: list[str],
    timeout: float,
    *,
    cwd: Path | None = None,
    env: dict[str, str] | None = None,
) -> tuple[bytes, bytes, int] | str:
    try:
        proc = await asyncio.create_subprocess_exec(
            *argv,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
            cwd=str(cwd) if cwd is not None else None,
            env=env,
            **process_group_kwargs(),
        )
    except Exception as exc:
        return f"Failed to start subprocess ({type(exc).__name__}: {exc!r})"

    try:
        stdout_b, stderr_b = await asyncio.wait_for(proc.communicate(), timeout=timeout)
    except asyncio.CancelledError:
        await asyncio.shield(kill_and_reap(proc))
        raise
    except TimeoutError:
        await kill_and_reap(proc)
        return f"bash_execute timed out after {timeout}s"

    return stdout_b or b"", stderr_b or b"", proc.returncode if proc.returncode is not None else 0


def _run_sync(
    argv: list[str],
    timeout: float,
    *,
    cwd: Path | None = None,
    env: dict[str, str] | None = None,
    cancel_event,
) -> tuple[bytes, bytes, int] | str:
    """Thread-pool fallback for event loops that can't spawn subprocesses."""
    try:
        return communicate_cancellable(
            argv,
            timeout,
            cwd=cwd,
            env=env,
            cancel_event=cancel_event,
        )
    except subprocess.TimeoutExpired:
        return f"bash_execute timed out after {timeout}s"
    except Exception as exc:
        return f"Failed to start subprocess ({type(exc).__name__}: {exc!r})"

def register_bash_tools() -> None:
    register_tool(
        name="bash_execute",
        description=(
            "Execute a shell command in an isolated subprocess and return its "
            "stdout. Stderr, exit code, and truncation are reported in metadata. "
            "Default timeout 30s. Host secrets are stripped from the subprocess "
            "environment. Suitable for build commands, package management, file "
            "operations, and general scripting."
        ),
        args_model=BashExecuteArgs,
        func=bash_execute,
        dangerous=True,
        capabilities=frozenset({Capability.EXECUTE}),
    )
