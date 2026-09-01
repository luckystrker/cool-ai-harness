"""Git tools: full repository workflow via the git CLI.

All tools operate on the active run context's working directory
(``get_run_context().workdir``). They shell out to the ``git`` binary via
``asyncio.create_subprocess_exec``. Outputs are secret-masked before being
returned to the model.

Tools provided:
    git_status, git_clone, git_diff, git_log, git_blame,
    git_branch, git_commit, git_push
"""

from __future__ import annotations

import asyncio
import sys
from pathlib import Path
from typing import Literal

from app.core.config import get_settings
from app.security.capabilities import Capability
from app.security.secrets import mask_tool_output
from app.tools.base import ToolArgs, ToolResult, register_tool
from app.tools.context import get_run_context
from app.tools.subprocess_cancellation import (
    communicate_cancellable,
    kill_and_reap,
    process_group_kwargs,
    run_in_thread_cancellable,
)

_GIT_TIMEOUT = 60.0  # seconds; clone may need more — overridden per-tool


# --- Shared helper ---------------------------------------------------------


def _loop_supports_subprocess(loop: asyncio.AbstractEventLoop) -> bool:
    if sys.platform != "win32":
        return True
    return type(loop).__name__ == "ProactorEventLoop"


async def run_git(
    *args: str,
    cwd: Path | None = None,
    timeout: float = _GIT_TIMEOUT,
) -> ToolResult:
    """Run a git command and return stdout/stderr as a ToolResult.

    This is the shared helper used by both the agent tools and the workspace
    API endpoints.
    """
    workdir = cwd or get_run_context().workdir
    workdir.mkdir(parents=True, exist_ok=True)

    loop = asyncio.get_running_loop()
    if _loop_supports_subprocess(loop):
        result = await _run_git_async(args, workdir, timeout)
    else:
        result = await run_in_thread_cancellable(_run_git_sync, args, workdir, timeout)

    if isinstance(result, str):
        return ToolResult.err(result)

    stdout_b, stderr_b, returncode = result
    stdout = stdout_b.decode("utf-8", errors="replace").strip() if stdout_b else ""
    stderr = stderr_b.decode("utf-8", errors="replace").strip() if stderr_b else ""

    if returncode != 0:
        msg = stderr or stdout or f"git exited with code {returncode}"
        return ToolResult.err(mask_tool_output(msg), exit_code=returncode)

    return ToolResult.ok(
        mask_tool_output(stdout or "(success)"),
        stderr=mask_tool_output(stderr) if stderr else None,
        exit_code=0,
    )


async def _run_git_async(
    args: tuple[str, ...], cwd: Path, timeout: float
) -> tuple[bytes, bytes, int] | str:
    try:
        proc = await asyncio.create_subprocess_exec(
            "git",
            *args,
            cwd=str(cwd),
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
            **process_group_kwargs(),
        )
    except FileNotFoundError:
        return "git binary not found on PATH"
    except Exception as exc:
        return f"Failed to start git ({type(exc).__name__}: {exc!r})"

    try:
        stdout_b, stderr_b = await asyncio.wait_for(proc.communicate(), timeout=timeout)
    except asyncio.CancelledError:
        await asyncio.shield(kill_and_reap(proc))
        raise
    except TimeoutError:
        await kill_and_reap(proc)
        return f"git {args[0] if args else ''} timed out after {timeout}s"

    return stdout_b or b"", stderr_b or b"", proc.returncode if proc.returncode is not None else 0


def _run_git_sync(
    args: tuple[str, ...], cwd: Path, timeout: float, *, cancel_event
) -> tuple[bytes, bytes, int] | str:
    """Thread-pool fallback for event loops that can't spawn subprocesses."""
    import subprocess

    try:
        return communicate_cancellable(
            ["git", *args],
            timeout,
            cwd=cwd,
            env=None,
            cancel_event=cancel_event,
        )
    except FileNotFoundError:
        return "git binary not found on PATH"
    except subprocess.TimeoutExpired:
        return f"git {args[0] if args else ''} timed out after {timeout}s"
    except Exception as exc:
        return f"Failed to start git ({type(exc).__name__}: {exc!r})"

# --- git_status ------------------------------------------------------------


class GitStatusArgs(ToolArgs):
    pass


async def git_status() -> ToolResult:
    """Show the working tree status (porcelain format)."""
    return await run_git("status", "--porcelain=v1", "--branch")


# --- git_clone -------------------------------------------------------------


class GitCloneArgs(ToolArgs):
    url: str
    directory: str = "."


async def git_clone(*, url: str, directory: str = ".") -> ToolResult:
    """Clone a repository. Only https:// and ssh:// (git@) URLs are allowed."""
    # Validate URL scheme.
    url_lower = url.lower()
    if not (url_lower.startswith("https://") or url_lower.startswith("http://") or "@" in url):
        return ToolResult.err(
            "Only https:// or SSH (git@host:repo) clone URLs are supported."
        )

    # SSRF domain check for http(s) URLs.
    if url_lower.startswith("http"):
        from app.security.ssrf import check_url_safety

        settings = get_settings()
        allowed_domains = settings.network_allowed_domains or None
        safety = check_url_safety(
            url,
            allowed_domains=allowed_domains,
            block_private_ips=settings.ssrf_block_private_ips,
        )
        if not safety.safe:
            return ToolResult.err(f"Clone URL blocked (SSRF protection): {safety.reason}")

    workdir = get_run_context().workdir
    target = workdir / directory
    # Clone needs a longer timeout.
    return await run_git("clone", url, str(target), cwd=workdir, timeout=300.0)


# --- git_diff --------------------------------------------------------------


class GitDiffArgs(ToolArgs):
    staged: bool = False
    path: str = ""


async def git_diff(*, staged: bool = False, path: str = "") -> ToolResult:
    """Show changes between working tree and index (or staged changes)."""
    args = ["diff"]
    if staged:
        args.append("--cached")
    if path:
        args.extend(["--", path])
    return await run_git(*args)


# --- git_log ---------------------------------------------------------------


class GitLogArgs(ToolArgs):
    limit: int = 10
    oneline: bool = True


async def git_log(*, limit: int = 10, oneline: bool = True) -> ToolResult:
    """Show commit log."""
    args = ["log", f"-{limit}"]
    if oneline:
        args.append("--oneline")
    else:
        args.append("--format=%H %an %ad %s")
    return await run_git(*args)


# --- git_blame -------------------------------------------------------------


class GitBlameArgs(ToolArgs):
    path: str
    line_start: int | None = None
    line_end: int | None = None


async def git_blame(
    *, path: str, line_start: int | None = None, line_end: int | None = None
) -> ToolResult:
    """Show what revision and author last modified each line of a file."""
    args = ["blame"]
    if line_start is not None and line_end is not None:
        args.extend(["-L", f"{line_start},{line_end}"])
    elif line_start is not None:
        args.extend(["-L", f"{line_start}"])
    args.append(path)
    return await run_git(*args)


# --- git_branch ------------------------------------------------------------


class GitBranchArgs(ToolArgs):
    action: Literal["list", "create", "checkout", "delete"] = "list"
    name: str | None = None


async def git_branch(*, action: str = "list", name: str | None = None) -> ToolResult:
    """Manage branches: list, create, checkout, or delete."""
    if action == "list":
        return await run_git("branch", "-a", "--format=%(refname:short) %(HEAD)")
    if action == "create":
        if not name:
            return ToolResult.err("Branch name is required for 'create' action")
        return await run_git("branch", name)
    if action == "checkout":
        if not name:
            return ToolResult.err("Branch name is required for 'checkout' action")
        return await run_git("checkout", name)
    if action == "delete":
        if not name:
            return ToolResult.err("Branch name is required for 'delete' action")
        return await run_git("branch", "-D", name)
    return ToolResult.err(f"Unknown action: {action}. Use list|create|checkout|delete")


# --- git_commit ------------------------------------------------------------


class GitCommitArgs(ToolArgs):
    message: str
    add_all: bool = True


async def git_commit(*, message: str, add_all: bool = True) -> ToolResult:
    """Commit changes. Optionally stages all changes first (git add -A)."""
    if add_all:
        add_result = await run_git("add", "-A")
        if add_result.is_error:
            return add_result
    return await run_git("commit", "-m", message)


# --- git_push --------------------------------------------------------------


class GitPushArgs(ToolArgs):
    remote: str = "origin"
    branch: str | None = None
    force: bool = False


async def git_push(
    *, remote: str = "origin", branch: str | None = None, force: bool = False
) -> ToolResult:
    """Push commits to a remote. Force push is flagged as dangerous."""
    args = ["push", remote]
    if branch:
        args.append(branch)
    if force:
        args.append("--force")
    return await run_git(*args, timeout=120.0)


# --- Registration ----------------------------------------------------------


def register_git_tools() -> None:
    register_tool(
        name="git_status",
        description=(
            "Show the working tree status: current branch, staged, modified, "
            "and untracked files (porcelain format)."
        ),
        args_model=GitStatusArgs,
        func=git_status,
        capabilities=frozenset({Capability.GIT}),
    )
    register_tool(
        name="git_clone",
        description=(
            "Clone a git repository into the workspace. Supports https:// and "
            "SSH (git@) URLs. SSRF-protected for HTTP(S) URLs."
        ),
        args_model=GitCloneArgs,
        func=git_clone,
        capabilities=frozenset({Capability.GIT, Capability.NETWORK}),
    )
    register_tool(
        name="git_diff",
        description=(
            "Show changes between working tree and index. Set staged=True to "
            "see staged (cached) changes. Optionally filter by path."
        ),
        args_model=GitDiffArgs,
        func=git_diff,
        capabilities=frozenset({Capability.GIT}),
    )
    register_tool(
        name="git_log",
        description="Show the commit log. Defaults to last 10 commits in oneline format.",
        args_model=GitLogArgs,
        func=git_log,
        capabilities=frozenset({Capability.GIT}),
    )
    register_tool(
        name="git_blame",
        description=(
            "Show what revision and author last modified each line of a file. "
            "Optionally restrict to a line range."
        ),
        args_model=GitBlameArgs,
        func=git_blame,
        capabilities=frozenset({Capability.GIT}),
    )
    register_tool(
        name="git_branch",
        description=(
            "Manage git branches: list all branches, create a new one, "
            "checkout (switch to) a branch, or delete a branch."
        ),
        args_model=GitBranchArgs,
        func=git_branch,
        capabilities=frozenset({Capability.GIT}),
    )
    register_tool(
        name="git_commit",
        description=(
            "Commit changes with a message. By default stages all changes "
            "(git add -A) before committing. Set add_all=False to commit "
            "only already-staged files."
        ),
        args_model=GitCommitArgs,
        func=git_commit,
        capabilities=frozenset({Capability.GIT}),
    )
    register_tool(
        name="git_push",
        description=(
            "Push commits to a remote repository. Force push is available "
            "but flagged as dangerous. Default remote is 'origin'."
        ),
        args_model=GitPushArgs,
        func=git_push,
        dangerous=True,
        capabilities=frozenset({Capability.GIT, Capability.NETWORK}),
    )
