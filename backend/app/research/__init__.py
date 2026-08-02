"""Deep Research workflow subsystem (Фаза 4 — Deep Research).

A durable, observable research pipeline: topic decomposition, parallel
researcher subagents, structured sources with confidence/conflict markers, and
a synthesized report with clickable citations stored as an artifact.
"""

from app.research.orchestrator import (
    cancel_research_run,
    execute_research,
    get_research_run,
    list_research_runs,
    rerun_research,
    run_research_for_task,
    run_research_inline,
    start_research,
)
from app.research.registry import research_registry

__all__ = [
    "cancel_research_run",
    "execute_research",
    "get_research_run",
    "list_research_runs",
    "rerun_research",
    "research_registry",
    "run_research_for_task",
    "run_research_inline",
    "start_research",
]
