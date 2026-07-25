"""Inspector service: run timeline reconstruction, comparison, and replay.

Builds on the append-only ``run_events`` log (Фаза 1.5 §1) to provide
developer-facing analysis:

- ``build_run_timeline``: reconstructs per-iteration timing/usage/tool-calls
  from the event log (primarily from ``llm_call_complete`` events).
- ``compare_runs``: computes metric deltas and per-iteration alignment between
  two runs.
- ``prepare_replay``: extracts the original user input and config from a
  completed run so it can be re-executed with different parameters.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any

from sqlmodel import Session, select

from app.models import AgentRun, Conversation, RunEvent
from app.models import Message as MessageRow


@dataclass
class IterationInfo:
    """Reconstructed per-iteration detail."""

    iteration: int
    duration_ms: int | None = None
    usage: dict[str, Any] | None = None
    model: str | None = None
    tool_calls: list[dict[str, Any]] = field(default_factory=list)
    finish_reason: str | None = None


@dataclass
class TimelineResult:
    """Full timeline for a run."""

    iterations: list[IterationInfo] = field(default_factory=list)
    total_duration_ms: int | None = None


@dataclass
class ComparisonResult:
    """Side-by-side comparison of two runs."""

    delta_tokens: int = 0
    delta_cost_usd: float | None = None
    delta_iterations: int = 0
    delta_duration_ms: int | None = None
    iterations_a: list[IterationInfo] = field(default_factory=list)
    iterations_b: list[IterationInfo] = field(default_factory=list)


@dataclass
class ReplayContext:
    """Everything needed to replay a run."""

    conversation_id: int
    user_input: str | None
    model: str | None
    system_prompt: str | None
    temperature: float | None
    tool_names: list[str] | None


def build_run_timeline(session: Session, run_id: int) -> TimelineResult | None:
    """Reconstruct a structured per-iteration timeline from the event log.

    Groups events by iteration using ``llm_call_complete`` events as iteration
    boundaries. Tool calls between two ``llm_call_complete`` events belong to
    the preceding iteration.

    Returns None if the run has no events.
    """
    events: list[RunEvent] = list(
        session.exec(
            select(RunEvent).where(RunEvent.run_id == run_id).order_by(RunEvent.seq)  # type: ignore[arg-type]
        ).all()
    )
    if not events:
        return None

    iterations: list[IterationInfo] = []
    current: IterationInfo | None = None
    total_duration_ms = 0

    for ev in events:
        payload = ev.payload or {}

        if ev.kind == "llm_call_complete":
            # Finalize previous iteration if it exists without a finish.
            iteration_num = payload.get("iteration", len(iterations) + 1)
            duration = payload.get("duration_ms")
            info = IterationInfo(
                iteration=iteration_num,
                duration_ms=duration,
                usage=payload.get("usage"),
                model=payload.get("model"),
            )
            if duration is not None:
                total_duration_ms += duration
            current = info
            iterations.append(info)

        elif ev.kind == "tool_call_start" and current is not None:
            current.tool_calls.append({
                "id": payload.get("id"),
                "name": payload.get("name"),
                "arguments": payload.get("arguments"),
            })

        elif ev.kind == "finish":
            # Attach finish reason to the last iteration.
            reason = payload.get("reason")
            if iterations:
                iterations[-1].finish_reason = reason
            # Use the overall elapsed_ms if we have no per-iteration sum.
            elapsed = payload.get("elapsed_ms")
            if elapsed is not None and total_duration_ms == 0:
                total_duration_ms = elapsed

    return TimelineResult(
        iterations=iterations,
        total_duration_ms=total_duration_ms or None,
    )


def compare_runs(session: Session, run_a_id: int, run_b_id: int) -> ComparisonResult | None:
    """Compare two runs: metric deltas + per-iteration timelines.

    Returns None if either run doesn't exist.
    """
    run_a = session.get(AgentRun, run_a_id)
    run_b = session.get(AgentRun, run_b_id)
    if run_a is None or run_b is None:
        return None

    usage_a = run_a.usage or {}
    usage_b = run_b.usage or {}

    tokens_a = usage_a.get("total_tokens", 0) or 0
    tokens_b = usage_b.get("total_tokens", 0) or 0

    cost_a = usage_a.get("cost_usd")
    cost_b = usage_b.get("cost_usd")
    delta_cost: float | None = None
    if cost_a is not None and cost_b is not None:
        delta_cost = cost_b - cost_a

    # Wall-clock duration from started_at/finished_at.
    delta_duration: int | None = None
    dur_a = _run_duration_ms(run_a)
    dur_b = _run_duration_ms(run_b)
    if dur_a is not None and dur_b is not None:
        delta_duration = dur_b - dur_a

    timeline_a = build_run_timeline(session, run_a_id)
    timeline_b = build_run_timeline(session, run_b_id)

    return ComparisonResult(
        delta_tokens=tokens_b - tokens_a,
        delta_cost_usd=delta_cost,
        delta_iterations=run_b.iterations - run_a.iterations,
        delta_duration_ms=delta_duration,
        iterations_a=timeline_a.iterations if timeline_a else [],
        iterations_b=timeline_b.iterations if timeline_b else [],
    )


def prepare_replay(
    session: Session,
    run_id: int,
    *,
    model_override: str | None = None,
    system_prompt_override: str | None = None,
    temperature_override: float | None = None,
) -> ReplayContext | None:
    """Extract the context needed to replay a run.

    Finds the first user message that was part of this run's conversation
    (the input that triggered the run), plus the run's config snapshot.
    Overrides are applied on top.

    Returns None if the run doesn't exist.
    """
    run = session.get(AgentRun, run_id)
    if run is None:
        return None

    conv = session.get(Conversation, run.conversation_id)
    if conv is None:
        return None

    # Find the user message that triggered this run: the last user message
    # created before or at the run's started_at.
    user_msg = session.exec(
        select(MessageRow)
        .where(MessageRow.conversation_id == run.conversation_id)
        .where(MessageRow.role == "user")
        .where(MessageRow.created_at <= run.started_at)
        .order_by(MessageRow.created_at.desc())  # type: ignore[attr-defined]
    ).first()

    user_input = user_msg.content if user_msg else None

    # Extract config from the run snapshot.
    config = run.config or {}
    tool_names = config.get("tool_names")

    return ReplayContext(
        conversation_id=run.conversation_id,
        user_input=user_input,
        model=model_override or run.model,
        system_prompt=system_prompt_override,
        temperature=temperature_override,
        tool_names=tool_names,
    )


def _run_duration_ms(run: AgentRun) -> int | None:
    """Wall-clock duration of a run in milliseconds (None if not finished)."""
    if run.started_at is None or run.finished_at is None:
        return None
    delta = run.finished_at - run.started_at
    return int(delta.total_seconds() * 1000)
