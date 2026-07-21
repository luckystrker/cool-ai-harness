"""Tests for the tool registry and the built-in file/code tools."""

from __future__ import annotations

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
