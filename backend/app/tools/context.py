"""Per-run execution context for tools.

Tools historically read global settings (``get_settings().workspaces_dir``)
to decide where files live and where subprocesses run. With per-conversation
working directories and permissions we need a per-run override.

This module exposes a ``ContextVar[RunContext]`` that the agent runner sets
at the start of each turn (see ``run_conversation_turn``) and tools read via
``get_run_context()``. When no run is in flight (e.g. ad-hoc tool calls in
tests), the global settings are used as the fallback context.

ContextVar is safe here: the agent loop is single-threaded async, and each
concurrent turn sets its own token before yielding control.
"""

from __future__ import annotations

from contextvars import ContextVar
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from app.core.config import get_settings

# Effective tool permission map: tool name -> "allow" | "ask" | "deny".
# The special key "*" is the catch-all default. See app/agent/permissions.py.
PermissionsMap = dict[str, str]


@dataclass
class RunContext:
    """What a tool should see as the "current run" environment.

    Attributes:
        workdir: Working directory for file tools and subprocess cwd.
            File tools confine paths to this root.
        permissions: Effective permission map (already merged with globals).
            Tools don't read this directly; the executor uses it to gate
            execution. Kept here so audit/logging has access from one place.
    """

    workdir: Path
    permissions: PermissionsMap = field(default_factory=dict)

    def resolve_workdir(self) -> Path:
        """Return workdir, ensuring it exists."""
        self.workdir.mkdir(parents=True, exist_ok=True)
        return self.workdir


# ContextVar default is None; ``get_run_context`` falls back to global settings.
_run_context: ContextVar[RunContext | None] = ContextVar("run_context", default=None)


def _default_context() -> RunContext:
    """Build a RunContext from global settings (used when no run is active)."""
    settings = get_settings()
    workdir = (
        Path(settings.default_working_directory)
        if settings.default_working_directory
        else Path(settings.workspaces_dir)
    )
    return RunContext(workdir=workdir, permissions=dict(settings.default_tool_permissions))


def get_run_context() -> RunContext:
    """Return the active RunContext, or a global-settings fallback.

    Tools should always call this rather than reading ``get_settings()``
    directly, so per-conversation overrides take effect.
    """
    ctx = _run_context.get()
    return ctx if ctx is not None else _default_context()


def set_run_context(ctx: RunContext | None) -> Any:
    """Set the active RunContext for the current async context.

    Returns the token to pass to :func:`reset_run_context`. Pass ``None`` to
    explicitly fall back to the global-settings context.
    """
    return _run_context.set(ctx)


def reset_run_context(token: Any) -> None:
    """Reset the RunContext to its previous value using a token from set()."""
    _run_context.reset(token)
