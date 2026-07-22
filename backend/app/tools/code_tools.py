"""Sandboxed Python execution tool.

Runs untrusted Python in a subprocess with timeouts and output limits. For
the MVP we rely on process isolation + resource caps; full Docker-container
sandboxing arrives in Фаза 4 (code-task workflow) when we also support Bash
and persistent workspaces with git.

Note: process isolation is *not* a true security sandbox. The MVP is
single-user and trusted; do not expose this tool to untrusted multi-tenant
traffic before the Docker sandbox lands.
"""

from __future__ import annotations

import asyncio
import sys
import textwrap
from pathlib import Path

from app.tools.base import ToolArgs, ToolResult, register_tool

# Capture stdout/stderr from a heredoc'd script.
_RUNNER_TEMPLATE = textwrap.dedent(
    """
    import sys
    _stdout, _stderr = sys.stdout, sys.stderr
    _out_buf, _err_buf = [], []
    class _Tee:
        def __init__(self, real, buf): self.real, self.buf = real, buf
        def write(self, s): self.real.write(s); self.buf.append(s)
        def flush(self): self.real.flush()
    sys.stdout = _Tee(_stdout, _out_buf)
    sys.stderr = _Tee(_stderr, _err_buf)
    try:
        __code__ = compile('''__USER_CODE__''', '<python_execute>', 'exec')
        exec(__code__, {'__name__': '__main__'})
    except Exception as e:
        import traceback
        sys.stderr.write(traceback.format_exc())
    finally:
        sys.stdout, sys.stderr = _stdout, _stderr
        import json
        json.dump({'stdout': ''.join(_out_buf), 'stderr': ''.join(_err_buf)}, sys.__stdout__)
"""
)


def _subprocess_argv(runner: str) -> list[str]:
    """Build the argv for launching the runner script.

    On Windows the process may be running under ``pythonw.exe`` (no console) or
    a launcher that ``create_subprocess_exec`` can't spawn directly — prefer the
    matching console ``python.exe`` when one sits next to the current
    interpreter. Returns the argv list to pass to ``create_subprocess_exec``.
    """
    exe = sys.executable
    if exe.lower().endswith("pythonw.exe"):
        import os

        console = exe[: -len("pythonw.exe")] + "python.exe"
        if os.path.exists(console):
            exe = console
    return [exe, "-c", runner]


class PythonExecuteArgs(ToolArgs):
    code: str
    timeout: float = 15.0
    max_output_chars: int = 20_000


async def python_execute(
    *, code: str, timeout: float = 15.0, max_output_chars: int = 20_000
) -> ToolResult:
    """Run Python code in a subprocess; return captured stdout/stderr."""
    # Escape triple-quotes so user code can't break out of the heredoc.
    safe_code = code.replace("'''", "\\'\\'\\'")
    runner = _RUNNER_TEMPLATE.replace("__USER_CODE__", safe_code)
    argv = _subprocess_argv(runner)

    loop = asyncio.get_running_loop()
    # Resolve the working directory from the active run context (honors the
    # per-conversation override; falls back to global workspaces_dir).
    from app.tools.context import get_run_context

    workdir = get_run_context().workdir
    # On Windows, a SelectorEventLoop (e.g. some uvicorn/uvloop setups) cannot
    # spawn subprocesses via the asyncio API — create_subprocess_exec raises
    # NotImplementedError with an empty message. Fall back to running a plain
    # subprocess in a worker thread, which works on any event loop.
    if _loop_supports_subprocess(loop):
        result = await _run_subprocess_async(argv, timeout, cwd=workdir)
    else:
        result = await asyncio.to_thread(_run_subprocess_sync, argv, timeout, cwd=workdir)

    if isinstance(result, str):
        return ToolResult.err(result)

    stdout_b, stderr_b, returncode = result
    stdout = stdout_b.decode("utf-8", errors="replace") if stdout_b else ""
    stderr = stderr_b.decode("utf-8", errors="replace") if stderr_b else ""

    truncated = False
    if len(stdout) > max_output_chars:
        stdout = stdout[:max_output_chars] + f"\n[... truncated at {max_output_chars} chars]"
        truncated = True

    return ToolResult.ok(
        stdout.strip() or "(no stdout)",
        stderr=stderr.strip() or None,
        exit_code=returncode,
        truncated=truncated,
    )


def _loop_supports_subprocess(loop: asyncio.AbstractEventLoop) -> bool:
    """True if the running loop can spawn subprocesses via the asyncio API.

    On Windows, ProactorEventLoop supports subprocesses; SelectorEventLoop does
    not (its ``subprocess_exec`` raises NotImplementedError, even though the
    method exists). Detect by class name rather than duck-typing. Unix loops do.
    """
    if sys.platform != "win32":
        return True
    loop_kind = type(loop).__name__
    return loop_kind == "ProactorEventLoop"


async def _run_subprocess_async(
    argv: list[str], timeout: float, *, cwd: Path | None = None
) -> tuple[bytes, bytes, int] | str:
    try:
        proc = await asyncio.create_subprocess_exec(
            *argv,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
            cwd=str(cwd) if cwd is not None else None,
        )
    except Exception as exc:
        loop_kind = type(asyncio.get_running_loop()).__name__
        return (
            f"Failed to start subprocess ({type(exc).__name__}: {exc!r}); "
            f"interpreter={sys.executable!r}; loop={loop_kind}"
        )

    try:
        stdout_b, stderr_b = await asyncio.wait_for(proc.communicate(), timeout=timeout)
    except TimeoutError:
        proc.kill()
        await proc.wait()
        return f"python_execute timed out after {timeout}s"

    return stdout_b or b"", stderr_b or b"", proc.returncode if proc.returncode is not None else 0


def _run_subprocess_sync(
    argv: list[str], timeout: float, *, cwd: Path | None = None
) -> tuple[bytes, bytes, int] | str:
    """Thread-pool fallback: run the subprocess synchronously via subprocess.run.

    Used when the running event loop can't spawn subprocesses (Windows
    SelectorEventLoop). Runs off-loop so it never blocks the loop.
    """
    import subprocess

    try:
        completed = subprocess.run(
            argv,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=timeout,
            check=False,
            cwd=str(cwd) if cwd is not None else None,
        )
    except subprocess.TimeoutExpired:
        return f"python_execute timed out after {timeout}s"
    except Exception as exc:
        return f"Failed to start subprocess ({type(exc).__name__}: {exc!r}); interpreter={sys.executable!r}"

    return completed.stdout or b"", completed.stderr or b"", completed.returncode


def register_code_tools() -> None:
    register_tool(
        name="python_execute",
        description=(
            "Execute Python 3 code in an isolated subprocess and return its "
            "stdout. Stderr, exit code, and truncation are reported in "
            "metadata. Default timeout 15s. Suitable for calculations, data "
            "processing, and quick experimentation — NOT a security sandbox."
        ),
        args_model=PythonExecuteArgs,
        func=python_execute,
        dangerous=True,
    )
