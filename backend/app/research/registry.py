"""In-memory registry of active research runs (cancellation).

Mirrors ``app.agent.subagents.SubagentRegistry``: one process-wide singleton
tracking the asyncio tasks running ``execute_research`` so the API layer can
cancel a live deep-research workflow. Cancelling the outer task cascades into
the researcher subagent tasks it spawned (they handle CancelledError and mark
themselves cancelled).
"""

from __future__ import annotations

import asyncio

from app.core.logging import get_logger

log = get_logger(__name__)


class ResearchRegistry:
    """In-memory registry tracking active research asyncio tasks."""

    def __init__(self) -> None:
        self._tasks: dict[int, asyncio.Task] = {}

    def register(self, research_run_id: int, task: asyncio.Task) -> None:
        self._tasks[research_run_id] = task

    def unregister(self, research_run_id: int) -> None:
        self._tasks.pop(research_run_id, None)

    def cancel(self, research_run_id: int) -> bool:
        task = self._tasks.get(research_run_id)
        if task is not None and not task.done():
            task.cancel()
            return True
        return False

    def is_active(self, research_run_id: int) -> bool:
        task = self._tasks.get(research_run_id)
        return task is not None and not task.done()

    @property
    def active_ids(self) -> list[int]:
        return [k for k, v in self._tasks.items() if not v.done()]


# Global singleton.
research_registry = ResearchRegistry()
