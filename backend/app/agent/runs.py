"""In-process registry of active agent runs (cancellation + observability).

When a run is ``cancellable`` (interactive chat over SSE/WebSocket), the
executor registers it here so an external ``cancel`` request — from the cancel
API endpoint, or a client disconnect — can signal the loop to stop. The loop
checks ``is_cancelled`` at the top of each iteration and before each tool call,
then emits a ``finish(reason="cancelled")`` and returns.

Design mirrors ``app/agent/approvals.py``: one process-wide singleton, pending
state held in ``asyncio`` primitives bound to the running loop. The MVP is
single-user / single-process; multi-process deploys would need a shared store
(Redis) — out of scope here, same as approvals.
"""

from __future__ import annotations

import asyncio
from dataclasses import dataclass

from app.core.logging import get_logger

log = get_logger(__name__)


@dataclass
class _ActiveRun:
    run_id: int
    conversation_id: int | None
    cancel_event: asyncio.Event


class RunRegistry:
    """Tracks active (cancellable) runs by run_id, keyed for cancel + lookup."""

    def __init__(self) -> None:
        self._active: dict[int, _ActiveRun] = {}

    def is_active(self, run_id: int) -> bool:
        """Whether a run is currently registered (running and cancellable)."""
        return run_id in self._active

    def register(self, run_id: int, *, conversation_id: int | None = None) -> asyncio.Event:
        """Register a run as active and return its cancel ``Event``.

        Re-registering the same run_id returns the existing event — ids are
        unique while a run is live. The executor awaits/periodically checks the
        returned event to detect cancellation.
        """
        existing = self._active.get(run_id)
        if existing is not None:
            return existing.cancel_event
        # asyncio.Event binds to the running loop on first use; register() is
        # only ever called from within the executor's loop (see run_conversation_turn).
        cancel_event = asyncio.Event()
        self._active[run_id] = _ActiveRun(
            run_id=run_id, conversation_id=conversation_id, cancel_event=cancel_event
        )
        log.info("run.registered", run_id=run_id, conversation_id=conversation_id)
        return cancel_event

    def is_cancelled(self, run_id: int) -> bool:
        """True if the run was cancelled (or never registered)."""
        active = self._active.get(run_id)
        return active is None or active.cancel_event.is_set()

    def cancel(self, run_id: int) -> bool:
        """Signal cancellation for a run.

        Returns True if a registered run was found and signalled, False if there
        was nothing to cancel (already finished / unknown id) — the endpoint
        turns that into a 404.
        """
        active = self._active.get(run_id)
        if active is None:
            return False
        active.cancel_event.set()
        log.info("run.cancel_signalled", run_id=run_id)
        return True

    def cancel_for_conversation(self, conversation_id: int) -> int:
        """Cancel every active run for a conversation (e.g. client disconnect).

        Returns the number of runs cancelled. Used when an SSE/WS client goes
        away so the loop doesn't keep working for a dead client.
        """
        cancelled = 0
        for active in list(self._active.values()):
            if active.conversation_id == conversation_id:
                active.cancel_event.set()
                cancelled += 1
        if cancelled:
            log.info(
                "run.cancel_for_conversation",
                conversation_id=conversation_id,
                count=cancelled,
            )
        return cancelled

    def unregister(self, run_id: int) -> None:
        """Drop a run from the registry (called from the executor's finally).

        Safe to call for an unregistered id (no-op). After this the run is no
        longer cancellable — which is correct, since the executor has exited.
        """
        removed = self._active.pop(run_id, None)
        if removed is not None:
            log.info("run.unregistered", run_id=run_id)

    def clear(self) -> None:
        """Remove everything. Intended for tests."""
        self._active.clear()


# Process-wide singleton. Import this where you need to register/cancel runs.
run_registry = RunRegistry()


__all__ = ["RunRegistry", "run_registry"]
