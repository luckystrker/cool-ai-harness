"""In-process registry of pending tool-call approvals.

When the executor hits an ``ask`` tool, it emits a ``tool_approval_request``
event and needs to *block* until the client resolves it. Because the SSE stream
is one-way, the client POSTs its decision to the approval endpoint, which calls
into this registry.

Design:
    - One process-wide singleton (``approval_registry``). The MVP is single-user
      / single-process, so this is fine. Multi-process deploys would need a
      shared store (Redis) — out of scope for now.
    - Each pending call is an ``asyncio.Future`` keyed by ``call_id``. The
      executor awaits it; the endpoint resolves it.
    - Futures are created lazily inside the running loop (``asyncio.Future``)
      so they're bound to the same loop the executor awaits on.
    - ``wait_for`` in the executor enforces a timeout so a forgotten prompt
      doesn't hang a turn forever (see ``ApprovalRegistry.register``).
"""

from __future__ import annotations

import asyncio
from dataclasses import dataclass

from app.core.logging import get_logger

log = get_logger(__name__)

# How long the executor will wait for a human decision before auto-denying.
DEFAULT_APPROVAL_TIMEOUT_S: float = 300.0


@dataclass
class _Pending:
    future: asyncio.Future[bool]
    conversation_id: int | None


class ApprovalRegistry:
    """Tracks pending tool-call approvals by call_id."""

    def __init__(self) -> None:
        self._pending: dict[str, _Pending] = {}

    def has(self, call_id: str) -> bool:
        return call_id in self._pending

    def register(self, call_id: str, *, conversation_id: int | None = None) -> asyncio.Future[bool]:
        """Create (or reuse) a pending approval for ``call_id``.

        Returns a Future the executor awaits. The Future resolves to ``True``
        (approved) or ``False`` (denied/timeout). Re-registering the same id
        returns the existing Future — call_ids are unique within a turn.
        """
        existing = self._pending.get(call_id)
        if existing is not None:
            return existing.future
        loop = asyncio.get_running_loop()
        future: asyncio.Future[bool] = loop.create_future()
        self._pending[call_id] = _Pending(future=future, conversation_id=conversation_id)
        log.info("approval.registered", call_id=call_id, conversation_id=conversation_id)
        return future

    def resolve(self, call_id: str, approved: bool) -> bool:
        """Resolve a pending approval from the client's decision.

        Returns True if a pending approval was found and resolved, False if
        there was nothing to resolve (the endpoint turns this into a 404).
        """
        pending = self._pending.pop(call_id, None)
        if pending is None:
            return False
        if not pending.future.done():
            pending.future.set_result(approved)
        log.info("approval.resolved", call_id=call_id, approved=approved)
        return True

    def cancel(self, call_id: str) -> None:
        """Cancel a pending approval (e.g. client disconnected mid-turn).

        Resolves the Future as denied so the executor's ``await`` unblocks
        rather than hanging on a dead client.
        """
        pending = self._pending.pop(call_id, None)
        if pending is None:
            return
        if not pending.future.done():
            pending.future.set_result(False)
        log.info("approval.cancelled", call_id=call_id)

    def cancel_for_conversation(self, conversation_id: int) -> int:
        """Cancel every pending approval for a conversation (e.g. disconnect).

        Returns the number of approvals cancelled. Used when an SSE client
        disconnects so we don't leave the executor awaiting a dead client.
        """
        cancelled = 0
        for call_id in list(self._pending):
            pending = self._pending[call_id]
            if pending.conversation_id == conversation_id:
                self.cancel(call_id)
                cancelled += 1
        return cancelled

    def clear(self) -> None:
        """Cancel everything. Intended for tests."""
        for call_id in list(self._pending):
            self.cancel(call_id)


# Process-wide singleton. Import this where you need to register/resolve.
approval_registry = ApprovalRegistry()


__all__ = [
    "DEFAULT_APPROVAL_TIMEOUT_S",
    "ApprovalRegistry",
    "approval_registry",
]
