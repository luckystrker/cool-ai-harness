"""Observability layer (Фаза 1.5 §6 — Debug / Inspector Mode).

Provides the ``InspectorRegistry``: a process-wide pub/sub hub that lets
live-inspection WebSocket subscribers observe events of an in-progress run
in real time. The runner publishes each event; subscribers receive them via
per-subscription ``asyncio.Queue`` instances.

Design mirrors ``app/agent/runs.py`` (``run_registry``): one singleton,
in-process, single-user MVP. Multi-process deploys would need Redis pub/sub.
"""

from __future__ import annotations

import asyncio
import contextlib
from typing import Any

from app.core.logging import get_logger

log = get_logger(__name__)


class InspectorRegistry:
    """Pub/sub hub for live run inspection.

    Subscribers call ``subscribe(run_id)`` to get an ``asyncio.Queue`` that
    receives every event published for that run. When the run finishes (or the
    subscriber disconnects), ``unsubscribe`` cleans up. A sentinel ``None`` is
    pushed to all queues when the run terminates so consumers can detect the end.
    """

    def __init__(self) -> None:
        # run_id -> list of subscriber queues
        self._subscribers: dict[int, list[asyncio.Queue[Any]]] = {}

    def subscribe(self, run_id: int) -> asyncio.Queue[Any]:
        """Register a new subscriber for a run's events.

        Returns an asyncio.Queue that will receive AgentEvent dicts (via
        ``event.to_dict()``) and a final ``None`` sentinel when the run ends.
        Bounded to 1000 events to prevent unbounded memory growth from slow
        WebSocket consumers.
        """
        queue: asyncio.Queue[Any] = asyncio.Queue(maxsize=1000)
        self._subscribers.setdefault(run_id, []).append(queue)
        log.debug("inspector.subscribed", run_id=run_id)
        return queue

    def unsubscribe(self, run_id: int, queue: asyncio.Queue[Any]) -> None:
        """Remove a subscriber queue. Safe to call multiple times."""
        subs = self._subscribers.get(run_id)
        if subs is None:
            return
        with contextlib.suppress(ValueError):
            subs.remove(queue)
        if not subs:
            del self._subscribers[run_id]
        log.debug("inspector.unsubscribed", run_id=run_id)

    def publish(self, run_id: int, event_dict: dict[str, Any]) -> None:
        """Broadcast an event dict to all subscribers of a run.

        Called from the runner after each yielded event. Non-blocking: puts
        into each queue without waiting. If a queue is full (slow consumer),
        the event is dropped for that subscriber rather than blocking the loop.
        """
        subs = self._subscribers.get(run_id)
        if not subs:
            return
        for q in subs:
            try:
                q.put_nowait(event_dict)
            except asyncio.QueueFull:
                log.warning("inspector.queue_full_dropped", run_id=run_id)

    def notify_finished(self, run_id: int) -> None:
        """Signal all subscribers that the run has ended (push None sentinel)."""
        subs = self._subscribers.get(run_id)
        if not subs:
            return
        for q in subs:
            with contextlib.suppress(asyncio.QueueFull):
                q.put_nowait(None)
        # Clean up — no more events will arrive for this run.
        del self._subscribers[run_id]

    def has_subscribers(self, run_id: int) -> bool:
        """Whether anyone is currently subscribed to a run's events."""
        return bool(self._subscribers.get(run_id))

    def clear(self) -> None:
        """Remove all subscriptions. Intended for tests."""
        self._subscribers.clear()


# Process-wide singleton.
inspector_registry = InspectorRegistry()

__all__ = ["InspectorRegistry", "inspector_registry"]
