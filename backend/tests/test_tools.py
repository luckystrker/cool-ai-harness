"""Tests for the tool registry and the built-in file/code tools."""

from __future__ import annotations

import asyncio
from pathlib import Path

import pytest

from app.tools import ToolArgs, ToolResult, get_registry, register_tool

# --- registry ---

def test_builtins_registered_on_import() -> None:
    names = set(get_registry().keys())
    assert {"read_file", "write_file", "list_files", "python_execute", "web_search", "web_fetch"} <= names


def test_register_tool_rejects_sync_func() -> None:
    class A(ToolArgs):
        x: int

    def sync_func(**_):  # not async
        pass

    with pytest.raises(TypeError, match="must be async"):
        register_tool(name="bad", description="", args_model=A, func=sync_func)  # type: ignore[arg-type]


def test_tool_run_validates_args() -> None:
    import asyncio

    class A(ToolArgs):
        n: int

    async def f(*, n: int) -> ToolResult:
        return ToolResult.ok(str(n * 2))

    t = register_tool(name="dbl", description="doubles", args_model=A, func=f)

    out = asyncio.run(t.run({"n": 21}))
    assert out.output == "42"

    bad = asyncio.run(t.run({"n": "not-an-int"}))
    assert bad.is_error and "Invalid arguments" in (bad.error or "")


# --- file tools ---

@pytest.mark.asyncio
async def test_write_then_read_file(workspace: Path) -> None:
    from app.tools.file_tools import read_file, write_file

    r = await write_file(path="notes.txt", content="hello world")
    assert not r.is_error
    assert (workspace / "notes.txt").read_text() == "hello world"

    r = await read_file(path="notes.txt")
    assert r.output == "hello world"


@pytest.mark.asyncio
async def test_read_file_missing(workspace: Path) -> None:
    from app.tools.file_tools import read_file

    r = await read_file(path="nope.txt")
    assert r.is_error
    assert "not found" in r.error.lower()


@pytest.mark.asyncio
async def test_file_path_escape_blocked(workspace: Path) -> None:
    from app.tools.file_tools import read_file

    r = await read_file(path="../../etc/passwd")
    assert r.is_error
    assert "escapes" in r.error.lower()


@pytest.mark.asyncio
async def test_list_files(workspace: Path) -> None:
    from app.tools.file_tools import list_files, write_file

    await write_file(path="a.txt", content="a")
    await write_file(path="sub/b.txt", content="b")
    r = await list_files(path=".")
    assert "a.txt" in r.output
    assert "sub/" in r.output


# --- python_execute ---

@pytest.mark.asyncio
async def test_python_execute_basic() -> None:
    from app.tools.code_tools import python_execute

    r = await python_execute(code="print(2 + 2)")
    assert not r.is_error
    assert "4" in r.output


@pytest.mark.asyncio
async def test_python_execute_timeout() -> None:
    from app.tools.code_tools import python_execute

    r = await python_execute(code="import time; time.sleep(5)", timeout=0.3)
    assert r.is_error
    assert "timed out" in r.error.lower()


@pytest.mark.asyncio
async def test_python_execute_selector_loop_fallback() -> None:
    """Regression: on Windows, a SelectorEventLoop cannot spawn subprocesses
    (create_subprocess_exec raises NotImplementedError). python_execute must
    fall back to a worker-thread subprocess.run so it still works under
    servers that run a Selector loop (observed with uvicorn --reload).
    """
    import sys

    from app.tools.code_tools import _loop_supports_subprocess, python_execute

    if sys.platform != "win32":
        pytest.skip("Selector-loop subprocess limitation is Windows-specific")

    loop = asyncio.get_running_loop()
    # The default pytest loop on Windows may be either kind; the point is that
    # the tool works regardless of which loop it lands on.
    r = await python_execute(code="print(2 + 2)")
    assert not r.is_error, f"expected success, got: {r.error!r}"
    assert "4" in r.output
    # And the helper must correctly classify the running loop.
    assert _loop_supports_subprocess(loop) == (
        type(loop).__name__ == "ProactorEventLoop"
    )


# --- RunContext (working directory override) -------------------------------


@pytest.mark.asyncio
async def test_file_tools_honor_run_context_workdir(tmp_path: Path) -> None:
    """Setting a RunContext workdir redirects file tools there."""
    from app.tools.context import RunContext, reset_run_context, set_run_context
    from app.tools.file_tools import read_file, write_file

    custom = tmp_path / "agent-ws"
    token = set_run_context(RunContext(workdir=custom))
    try:
        r = await write_file(path="ctx.txt", content="data")
        assert not r.is_error
        # Written to the override dir, not the global workspace.
        assert (custom / "ctx.txt").read_text() == "data"
        r = await read_file(path="ctx.txt")
        assert r.output == "data"
    finally:
        reset_run_context(token)


@pytest.mark.asyncio
async def test_python_execute_honors_run_context_cwd(tmp_path: Path) -> None:
    """python_execute spawns its subprocess in the RunContext workdir."""
    from app.tools.code_tools import python_execute
    from app.tools.context import RunContext, reset_run_context, set_run_context

    custom = tmp_path / "cwd-ws"
    (custom).mkdir()
    # Drop a marker file the subprocess can read via cwd-relative path.
    (custom / "marker.txt").write_text("hello-cwd")
    token = set_run_context(RunContext(workdir=custom))
    try:
        r = await python_execute(code="print(open('marker.txt').read())")
        assert not r.is_error, f"got error: {r.error!r}"
        assert "hello-cwd" in r.output
    finally:
        reset_run_context(token)


@pytest.mark.asyncio
async def test_default_context_falls_back_to_settings(workspace: Path) -> None:
    """With no RunContext set, tools use the global settings workspace."""
    from app.tools.context import get_run_context
    from app.tools.file_tools import write_file

    ctx = get_run_context()
    assert ctx.workdir == workspace
    await write_file(path="fallback.txt", content="x")
    assert (workspace / "fallback.txt").exists()
