"""File tools, scoped to a workspace directory.

All paths are resolved relative to the configured ``workspaces_dir`` and
confined to it (no escaping via ``..`` or absolute paths). This keeps the
single-user MVP safe enough; full sandboxing (Docker container per agent)
arrives in Фаза 4 with the code-task workflow.
"""

from __future__ import annotations

import difflib
from pathlib import Path

from app.security.capabilities import Capability
from app.security.secrets import mask_tool_output
from app.tools.base import ToolArgs, ToolResult, register_tool
from app.tools.context import get_run_context


def _workspace_root() -> Path:
    """Workspace root for the active run (per-conversation override aware)."""
    return get_run_context().workdir


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
        # Mask any secrets that might be in the file content.
        safe_text = mask_tool_output(text + suffix)
        return ToolResult.ok(safe_text, bytes=len(data), truncated=truncated)
    except PermissionError as exc:
        return ToolResult.err(str(exc))
    except Exception as exc:
        return ToolResult.err(f"read_file failed: {exc}")


class WriteFileArgs(ToolArgs):
    path: str
    content: str
    append: bool = False


async def write_file(*, path: str, content: str, append: bool = False) -> ToolResult:
    """Write text to a file inside the workspace. Creates parent dirs.

    When overwriting an existing file, the result metadata includes a unified
    diff so the UI can show a preview of what changed.
    """
    try:
        full = _resolve(path)
        full.parent.mkdir(parents=True, exist_ok=True)

        # Capture the old content for a diff (only on overwrite, not append).
        old_text: str | None = None
        if not append and full.is_file():
            try:
                old_text = full.read_text(encoding="utf-8", errors="replace")
            except Exception:
                old_text = None

        mode = "a" if append else "w"
        with full.open(mode, encoding="utf-8") as f:
            f.write(content)

        # Build a unified diff if we overwrote an existing file.
        diff: str | None = None
        if old_text is not None and old_text != content:
            diff_lines = list(
                difflib.unified_diff(
                    old_text.splitlines(keepends=True),
                    content.splitlines(keepends=True),
                    fromfile=f"{path} (old)",
                    tofile=f"{path} (new)",
                    n=3,
                )
            )
            diff = "".join(diff_lines) if diff_lines else None

        return ToolResult.ok(
            f"Wrote {len(content)} chars to {path} ({mode} mode)",
            bytes=len(content.encode("utf-8")),
            diff=diff,
            created=old_text is None and not append,
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
        return ToolResult.ok(mask_tool_output(listing))
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
        capabilities=frozenset({Capability.READ}),
    )
    register_tool(
        name="write_file",
        description=(
            "Write text to a file in the agent's workspace. Creates parent "
            "directories. Set append=True to add to an existing file. "
            "When overwriting, a unified diff is included in the result metadata."
        ),
        args_model=WriteFileArgs,
        func=write_file,
        capabilities=frozenset({Capability.WRITE}),
    )
    register_tool(
        name="list_files",
        description="List files and directories under a path in the workspace.",
        args_model=ListFilesArgs,
        func=list_files,
        capabilities=frozenset({Capability.READ}),
    )
