"""Workspace utilities: git branch info, directory browsing, recent projects.

These endpoints power the composer toolbar in the frontend (working-directory
picker with recent projects, folder browser, and git branch badge).
"""

from __future__ import annotations

import asyncio
from pathlib import Path

from fastapi import APIRouter, Depends, HTTPException, Query
from sqlmodel import Session, select

from app.core.config import get_settings
from app.core.db import get_session
from app.models import Conversation

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
    except (OSError, asyncio.TimeoutError):
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
    if not path:
        target = Path.home()
    else:
        target = Path(path)

    if not target.is_dir():
        raise HTTPException(status_code=400, detail="Path is not a directory")

    dirs: list[str] = []
    try:
        for entry in sorted(target.iterdir(), key=lambda e: e.name.lower()):
            if entry.is_dir() and not entry.name.startswith("."):
                dirs.append(entry.name)
    except PermissionError:
        raise HTTPException(status_code=403, detail="Permission denied")

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
