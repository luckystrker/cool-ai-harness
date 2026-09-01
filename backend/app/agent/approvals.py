"""In-process registry of pending tool-call approvals.

When the executor hits an ``ask`` tool, it emits a ``tool_approval_request``
event and needs to *block* until the client resolves it. Because the SSE stream
is one-way, the client POSTs its decision to the approval endpoint, which calls
into this registry.

Design:
    - One process-wide singleton (``approval_registry``). The MVP is single-user
      / single-process, so this is fine. Multi-process deploys would need a
      shared store (Redis) — out of scope for now.
    - Each pending call is an ``asyncio.Future`` keyed by an unguessable,
      server-generated ``approval_id``. Model call ids are indexed only inside
      the actor/conversation/run scope.
    - Futures are created lazily inside the running loop (``asyncio.Future``)
      so they're bound to the same loop the executor awaits on.
    - ``wait_for`` in the executor enforces a timeout so a forgotten prompt
      doesn't hang a turn forever (see ``ApprovalRegistry.register``).
"""

from __future__ import annotations

import asyncio
import uuid
from dataclasses import dataclass

from app.core.logging import get_logger

log = get_logger(__name__)

# How long the executor will wait for a human decision before auto-denying.
DEFAULT_APPROVAL_TIMEOUT_S: float = 300.0


@dataclass
class _Pending:
    future: asyncio.Future[bool]
    approval_id: str
    call_id: str
    actor_id: int | None
    conversation_id: int | None
    run_id: int | None
    revision: int


@dataclass(frozen=True)
class ApprovalTicket:
    approval_id: str
    revision: int


class ApprovalRegistry:
    """Tracks pending approvals by server-generated approval identity."""

    def __init__(self) -> None:
        # Model-provided call ids are not globally unique. The primary key is
        # therefore always the unguessable, server-generated approval id.
        self._pending: dict[str, _Pending] = {}

    def has(
        self,
        approval_id: str,
        *,
        actor_id: int | None = None,
        conversation_id: int | None = None,
        run_id: int | None = None,
        expected_revision: int | None = None,
    ) -> bool:
        pending = self._find_by_approval(
            approval_id,
            actor_id=actor_id,
            conversation_id=conversation_id,
            run_id=run_id,
            expected_revision=expected_revision,
        )
        return pending is not None and not pending.future.done()

    def _find_by_approval(
        self,
        approval_id: str,
        *,
        actor_id: int | None = None,
        conversation_id: int | None = None,
        run_id: int | None = None,
        expected_revision: int | None = None,
    ) -> _Pending | None:
        pending = self._pending.get(approval_id)
        if pending is None:
            return None
        if actor_id is not None and pending.actor_id != actor_id:
            return None
        if conversation_id is not None and pending.conversation_id != conversation_id:
            return None
        if run_id is not None and pending.run_id != run_id:
            return None
        if expected_revision is not None and pending.revision != expected_revision:
            return None
        return pending

    def _find_by_call(
        self,
        call_id: str,
        *,
        actor_id: int | None,
        conversation_id: int | None,
        run_id: int | None,
    ) -> _Pending | None:
        return next(
            (
                candidate
                for candidate in self._pending.values()
                if candidate.call_id == call_id
                and candidate.actor_id == actor_id
                and candidate.conversation_id == conversation_id
                and candidate.run_id == run_id
            ),
            None,
        )

    def register(
        self,
        call_id: str,
        *,
        actor_id: int | None = None,
        conversation_id: int | None = None,
        run_id: int | None = None,
    ) -> asyncio.Future[bool]:
        """Create (or reuse) a pending approval for ``call_id``.

        Returns a Future the executor awaits. The Future resolves to ``True``
        (approved) or ``False`` (denied/timeout). Re-registering the same id
        returns the existing Future only within the same actor/conversation/run.
        """
        existing = self._find_by_call(
            call_id,
            actor_id=actor_id,
            conversation_id=conversation_id,
            run_id=run_id,
        )
        if existing is not None:
            return existing.future
        loop = asyncio.get_running_loop()
        future: asyncio.Future[bool] = loop.create_future()
        approval_id = f"approval-{uuid.uuid4()}"
        self._pending[approval_id] = _Pending(
            future=future,
            approval_id=approval_id,
            call_id=call_id,
            actor_id=actor_id,
            conversation_id=conversation_id,
            run_id=run_id,
            revision=1,
        )
        log.info(
            "approval.registered",
            approval_id=approval_id,
            call_id=call_id,
            actor_id=actor_id,
            conversation_id=conversation_id,
            run_id=run_id,
        )
        return future

    def ticket(
        self,
        call_id: str,
        *,
        actor_id: int | None = None,
        conversation_id: int | None = None,
        run_id: int | None = None,
    ) -> ApprovalTicket:
        pending = self._find_by_call(
            call_id,
            actor_id=actor_id,
            conversation_id=conversation_id,
            run_id=run_id,
        )
        if pending is None:
            raise KeyError(f"no pending approval for call {call_id}")
        return ApprovalTicket(approval_id=pending.approval_id, revision=pending.revision)

    def resolve(
        self,
        approval_id: str,
        approved: bool,
        *,
        expected_revision: int,
        actor_id: int | None = None,
        conversation_id: int | None = None,
        run_id: int | None = None,
    ) -> bool:
        """Resolve a scoped pending approval from the client's decision.

        Returns True if a pending approval was found and resolved, False if
        there was nothing to resolve (the endpoint turns this into a 404).
        """
        pending = self._find_by_approval(
            approval_id,
            actor_id=actor_id,
            conversation_id=conversation_id,
            run_id=run_id,
            expected_revision=expected_revision,
        )
        if pending is None or pending.future.done():
            return False
        # Keep the resolved Future registered until the executor consumes it.
        # Otherwise a fast client can resolve between the request event and
        # ``_wait_for_approval``, causing the executor to create a fresh Future
        # and wait until timeout.
        pending.future.set_result(approved)
        log.info(
            "approval.resolved",
            approval_id=approval_id,
            revision=expected_revision,
            approved=approved,
        )
        return True

    def future(
        self,
        approval_id: str,
        *,
        expected_revision: int,
        actor_id: int | None = None,
        conversation_id: int | None = None,
        run_id: int | None = None,
    ) -> asyncio.Future[bool]:
        pending = self._find_by_approval(
            approval_id,
            actor_id=actor_id,
            conversation_id=conversation_id,
            run_id=run_id,
            expected_revision=expected_revision,
        )
        if pending is None:
            raise KeyError(f"no pending approval {approval_id}")
        return pending.future

    def forget(self, approval_id: str) -> None:
        """Remove an approval after its executor has consumed the decision."""
        self._pending.pop(approval_id, None)

    def cancel(self, approval_id: str) -> None:
        """Cancel a pending approval (e.g. client disconnected mid-turn).

        Resolves the Future as denied so the executor's ``await`` unblocks
        rather than hanging on a dead client.
        """
        pending = self._pending.get(approval_id)
        if pending is None:
            return
        if not pending.future.done():
            pending.future.set_result(False)
        log.info("approval.cancelled", approval_id=approval_id)

    def cancel_for_conversation(self, conversation_id: int) -> int:
        """Cancel every pending approval for a conversation (e.g. disconnect).

        Returns the number of approvals cancelled. Used when an SSE client
        disconnects so we don't leave the executor awaiting a dead client.
        """
        cancelled = 0
        for approval_id in list(self._pending):
            pending = self._pending[approval_id]
            if pending.conversation_id == conversation_id:
                self.cancel(approval_id)
                cancelled += 1
        return cancelled

    def cancel_for_run(self, run_id: int, *, conversation_id: int | None = None) -> int:
        """Cancel approvals owned by one run, optionally verifying its conversation."""
        cancelled = 0
        for approval_id in list(self._pending):
            pending = self._pending[approval_id]
            if pending.run_id != run_id:
                continue
            if conversation_id is not None and pending.conversation_id != conversation_id:
                continue
            self.cancel(approval_id)
            cancelled += 1
        return cancelled

    def clear(self) -> None:
        """Cancel everything. Intended for tests."""
        for approval_id in list(self._pending):
            self.cancel(approval_id)
        self._pending.clear()


# Process-wide singleton. Import this where you need to register/resolve.
approval_registry = ApprovalRegistry()


__all__ = [
    "DEFAULT_APPROVAL_TIMEOUT_S",
    "ApprovalRegistry",
    "ApprovalTicket",
    "approval_registry",
]
