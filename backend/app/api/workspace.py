"""Workspace utilities: git branch info, directory browsing, recent projects.

These endpoints power the composer toolbar in the frontend (working-directory
picker with recent projects, folder browser, and git branch badge).

Фаза 4 adds richer git endpoints: status, log, branches, checkout.
"""

from __future__ import annotations

import asyncio
from pathlib import Path

from fastapi import APIRouter, Depends, HTTPException, Query
from sqlmodel import Session, select

from app.api.schemas import (
    GitBranchOut,
    GitCheckoutRequest,
    GitLogEntry,
    GitLogOut,
    GitStatusOut,
)
from app.core.config import get_settings
from app.core.db import get_session
from app.models import Conversation
from app.tools.git_tools import run_git

router = APIRouter()


@router.get("/workspace/git-info")
async def git_info(path: str = Query(..., description="Directory to inspect")) -> dict:
    """Return the current git branch for *path* (or is_git=false)."""
    target = Path(path)
    if not target.is_dir():
        raise HTTPException(status_code=400, detail="Path is not a directory")

    try:
        proc = await asyncio.create_subprocess_exec(
            "git",
            "rev-parse",
            "--abbrev-ref",
            "HEAD",
            cwd=str(target),
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
        )
        stdout, _ = await asyncio.wait_for(proc.communicate(), timeout=5)
    except (TimeoutError, OSError):
        return {"path": str(target), "is_git": False, "branch": None}

    if proc.returncode != 0:
        return {"path": str(target), "is_git": False, "branch": None}

    return {"path": str(target), "is_git": True, "branch": stdout.decode().strip()}


@router.get("/workspace/directories")
async def list_directories(
    path: str | None = Query(None, description="Parent directory to list; empty = home"),
) -> dict:
    """List sub-directories of *path* for the folder browser dialog."""
    settings = get_settings()
    target = Path.home() if not path else Path(path)

    if not target.is_dir():
        raise HTTPException(status_code=400, detail="Path is not a directory")

    dirs: list[str] = []
    try:
        for entry in sorted(target.iterdir(), key=lambda e: e.name.lower()):
            if entry.is_dir() and not entry.name.startswith("."):
                dirs.append(entry.name)
    except PermissionError:
        raise HTTPException(status_code=403, detail="Permission denied") from None

    return {
        "current": str(target),
        "parent": str(target.parent) if target.parent != target else None,
        "directories": dirs,
        "default": str(settings.workspaces_dir),
    }


@router.get("/workspace/recent")
def recent_directories(session: Session = Depends(get_session)) -> dict:
    """Distinct working directories across conversations + the global default."""
    settings = get_settings()
    stmt = select(Conversation.working_directory).where(
        Conversation.working_directory.is_not(None)  # type: ignore[union-attr]
    )
    rows = session.exec(stmt).all()
    # Deduplicate preserving newest-first order (rows come in insertion order).
    seen: set[str] = set()
    recent: list[str] = []
    for wd in reversed(rows):
        if wd and wd not in seen:
            seen.add(wd)
            recent.append(wd)

    default = str(settings.default_working_directory or settings.workspaces_dir)
    return {"recent": recent[:10], "default": default}


# --- Git endpoints (Фаза 4) ------------------------------------------------


def _validate_git_dir(path: str) -> Path:
    """Validate that path is a directory and return it."""
    target = Path(path)
    if not target.is_dir():
        raise HTTPException(status_code=400, detail="Path is not a directory")
    return target


@router.get("/workspace/git-status", response_model=GitStatusOut)
async def git_status_endpoint(
    path: str = Query(..., description="Repository directory"),
) -> GitStatusOut:
    """Parsed git status: branch, staged, modified, and untracked files."""
    target = _validate_git_dir(path)

    result = await run_git("status", "--porcelain=v1", "--branch", cwd=target)
    if result.is_error:
        return GitStatusOut(path=str(target), is_git=False)

    staged: list[str] = []
    modified: list[str] = []
    untracked: list[str] = []
    branch: str | None = None

    for line in result.output.splitlines():
        if line.startswith("## "):
            # e.g. "## main...origin/main"
            branch = line[3:].split("...")[0]
        elif line.startswith("?? "):
            untracked.append(line[3:])
        elif len(line) >= 3:
            x, y = line[0], line[1]
            filepath = line[3:]
            if x in ("A", "M", "R", "C"):
                staged.append(filepath)
            if y in ("M", "D"):
                modified.append(filepath)

    return GitStatusOut(
        path=str(target),
        is_git=branch is not None,
        branch=branch,
        staged=staged,
        modified=modified,
        untracked=untracked,
    )


@router.get("/workspace/git-log", response_model=GitLogOut)
async def git_log_endpoint(
    path: str = Query(..., description="Repository directory"),
    limit: int = Query(10, ge=1, le=100, description="Number of commits"),
) -> GitLogOut:
    """Recent commit log for a repository."""
    target = _validate_git_dir(path)

    result = await run_git(
        "log", f"-{limit}", "--format=%H|%an|%ad|%s", "--date=short", cwd=target
    )
    if result.is_error:
        return GitLogOut(path=str(target), commits=[])

    commits: list[GitLogEntry] = []
    for line in result.output.splitlines():
        parts = line.split("|", 3)
        if len(parts) == 4:
            commits.append(
                GitLogEntry(hash=parts[0], author=parts[1], date=parts[2], message=parts[3])
            )

    return GitLogOut(path=str(target), commits=commits)


@router.get("/workspace/git-branches", response_model=GitBranchOut)
async def git_branches_endpoint(
    path: str = Query(..., description="Repository directory"),
) -> GitBranchOut:
    """List all local branches and indicate the current one."""
    target = _validate_git_dir(path)

    result = await run_git("branch", "--format=%(refname:short)|%(HEAD)", cwd=target)
    if result.is_error:
        return GitBranchOut(path=str(target), branches=[], current=None)

    branches: list[str] = []
    current: str | None = None
    for line in result.output.splitlines():
        parts = line.split("|", 1)
        name = parts[0].strip()
        if name:
            branches.append(name)
            if len(parts) > 1 and parts[1].strip() == "*":
                current = name

    return GitBranchOut(path=str(target), branches=branches, current=current)


@router.post("/workspace/git-checkout")
async def git_checkout_endpoint(body: GitCheckoutRequest) -> dict:
    """Switch to a different branch."""
    target = _validate_git_dir(body.path)

    result = await run_git("checkout", body.branch, cwd=target)
    if result.is_error:
        raise HTTPException(status_code=400, detail=result.output)

    return {"path": str(target), "branch": body.branch, "status": "ok"}
