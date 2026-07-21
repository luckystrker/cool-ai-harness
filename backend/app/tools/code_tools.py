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

    try:
        proc = await asyncio.create_subprocess_exec(
            sys.executable,
            "-c",
            runner,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
        )
    except Exception as exc:
        return ToolResult.err(f"Failed to start subprocess: {exc}")

    try:
        stdout_b, stderr_b = await asyncio.wait_for(
            proc.communicate(), timeout=timeout
        )
    except TimeoutError:
        proc.kill()
        await proc.wait()
        return ToolResult.err(
            f"python_execute timed out after {timeout}s", timeout=True
        )

    stdout = stdout_b.decode("utf-8", errors="replace") if stdout_b else ""
    stderr = stderr_b.decode("utf-8", errors="replace") if stderr_b else ""

    truncated = False
    if len(stdout) > max_output_chars:
        stdout = stdout[:max_output_chars] + f"\n[... truncated at {max_output_chars} chars]"
        truncated = True

    return ToolResult.ok(
        stdout.strip() or "(no stdout)",
        stderr=stderr.strip() or None,
        exit_code=proc.returncode,
        truncated=truncated,
    )


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
