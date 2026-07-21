"""File tools, scoped to a workspace directory.

All paths are resolved relative to the configured ``workspaces_dir`` and
confined to it (no escaping via ``..`` or absolute paths). This keeps the
single-user MVP safe enough; full sandboxing (Docker container per agent)
arrives in Фаза 4 with the code-task workflow.
"""

from __future__ import annotations

from pathlib import Path

from app.core.config import get_settings
from app.tools.base import ToolArgs, ToolResult, register_tool


def _workspace_root() -> Path:
    return Path(get_settings().workspaces_dir)


def _resolve(path: str) -> Path:
    """Resolve ``path`` inside the workspace and enforce it stays there."""
    root = _workspace_root().resolve()
    full = (root / path).resolve()
    try:
        full.relative_to(root)
    except ValueError as exc:
        raise PermissionError(f"Path {path!r} escapes the workspace") from exc
    return full


class ReadFileArgs(ToolArgs):
    path: str
    max_bytes: int = 200_000


async def read_file(*, path: str, max_bytes: int = 200_000) -> ToolResult:
    """Read up to ``max_bytes`` bytes of a UTF-8 text file from the workspace."""
    try:
        full = _resolve(path)
        if not full.is_file():
            return ToolResult.err(f"File not found: {path}")
        data = full.read_bytes()
        truncated = len(data) > max_bytes
        text = data[:max_bytes].decode("utf-8", errors="replace")
        suffix = f"\n\n[... truncated, {len(data)} bytes total]" if truncated else ""
        return ToolResult.ok(text + suffix, bytes=len(data), truncated=truncated)
    except PermissionError as exc:
        return ToolResult.err(str(exc))
    except Exception as exc:
        return ToolResult.err(f"read_file failed: {exc}")


class WriteFileArgs(ToolArgs):
    path: str
    content: str
    append: bool = False


async def write_file(*, path: str, content: str, append: bool = False) -> ToolResult:
    """Write text to a file inside the workspace. Creates parent dirs."""
    try:
        full = _resolve(path)
        full.parent.mkdir(parents=True, exist_ok=True)
        mode = "a" if append else "w"
        with full.open(mode, encoding="utf-8") as f:
            f.write(content)
        return ToolResult.ok(
            f"Wrote {len(content)} chars to {path} ({mode} mode)",
            bytes=len(content.encode("utf-8")),
        )
    except PermissionError as exc:
        return ToolResult.err(str(exc))
    except Exception as exc:
        return ToolResult.err(f"write_file failed: {exc}")


class ListFilesArgs(ToolArgs):
    path: str = "."


async def list_files(*, path: str = ".") -> ToolResult:
    """List files and directories under ``path`` inside the workspace."""
    try:
        full = _resolve(path)
        if not full.exists():
            return ToolResult.err(f"Not found: {path}")
        if full.is_file():
            return ToolResult.ok(f"{path} (file)")
        entries = sorted(p.name + ("/" if p.is_dir() else "") for p in full.iterdir())
        listing = "\n".join(entries) if entries else "(empty)"
        return ToolResult.ok(listing)
    except PermissionError as exc:
        return ToolResult.err(str(exc))
    except Exception as exc:
        return ToolResult.err(f"list_files failed: {exc}")


def register_file_tools() -> None:
    register_tool(
        name="read_file",
        description=(
            "Read a UTF-8 text file from the agent's workspace. "
            "Returns the file contents (truncated above max_bytes)."
        ),
        args_model=ReadFileArgs,
        func=read_file,
    )
    register_tool(
        name="write_file",
        description=(
            "Write text to a file in the agent's workspace. Creates parent "
            "directories. Set append=True to add to an existing file."
        ),
        args_model=WriteFileArgs,
        func=write_file,
    )
    register_tool(
        name="list_files",
        description="List files and directories under a path in the workspace.",
        args_model=ListFilesArgs,
        func=list_files,
    )
