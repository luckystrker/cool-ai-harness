"""Inspector routes: run timeline, comparison, and replay (Фаза 1.5 §6).

Developer-facing endpoints for debugging agent runs:

  GET  /conversations/{conv}/runs/{run}/timeline  — per-iteration breakdown
  GET  /runs/compare?a={id}&b={id}                — side-by-side comparison
  POST /conversations/{conv}/runs/{run}/replay    — re-execute with overrides
"""

from __future__ import annotations

from fastapi import APIRouter, Depends, HTTPException, Query
from sqlmodel import Session

from app.agent.service import (
    append_message,
    create_run,
    get_conversation,
    get_run,
    list_run_events,
)
from app.api.schemas import (
    IterationDetail,
    ReplayRequest,
    ReplayResponse,
    RunComparison,
    RunDetail,
    RunEventOut,
    RunOut,
    RunTimeline,
)
from app.core.db import get_session
from app.observability.inspector import (
    build_run_timeline,
    compare_runs,
    prepare_replay,
)

router = APIRouter()


def _require_conversation(session: Session, conv_id: int) -> None:
    if get_conversation(session, conv_id) is None:
        raise HTTPException(status_code=404, detail="Conversation not found")


def _require_run(session: Session, conv_id: int, run_id: int):
    _require_conversation(session, conv_id)
    run = get_run(session, run_id)
    if run is None or run.conversation_id != conv_id:
        raise HTTPException(status_code=404, detail="Run not found")
    return run


def _run_to_out(run) -> RunOut:
    return RunOut(
        id=run.id,
        conversation_id=run.conversation_id,
        status=run.status,
        model=run.model,
        iterations=run.iterations,
        usage=run.usage,
        finish_reason=run.finish_reason,
        error=run.error,
        started_at=run.started_at,
        finished_at=run.finished_at,
        created_at=run.created_at,
        updated_at=run.updated_at,
    )


def _event_to_out(ev) -> RunEventOut:
    return RunEventOut(
        id=ev.id,
        run_id=ev.run_id,
        seq=ev.seq,
        kind=ev.kind,
        payload=ev.payload,
        created_at=ev.created_at,
    )


def _iteration_to_detail(info) -> IterationDetail:
    return IterationDetail(
        iteration=info.iteration,
        duration_ms=info.duration_ms,
        usage=info.usage,
        model=info.model,
        tool_calls=info.tool_calls,
        finish_reason=info.finish_reason,
    )


@router.get(
    "/conversations/{conv_id}/runs/{run_id}/timeline", response_model=RunTimeline
)
def get_run_timeline(
    conv_id: int, run_id: int, session: Session = Depends(get_session)
) -> RunTimeline:
    """Structured per-iteration timeline for a run.

    Reconstructs timing, token usage, and tool calls per LLM iteration from
    the append-only event log.
    """
    run = _require_run(session, conv_id, run_id)
    events = [_event_to_out(e) for e in list_run_events(session, run_id=run_id)]
    run_detail = RunDetail(
        **_run_to_out(run).model_dump(),
        config=run.config,
        checkpoint=run.checkpoint,
        events=events,
    )

    timeline = build_run_timeline(session, run_id)
    iterations = (
        [_iteration_to_detail(i) for i in timeline.iterations] if timeline else []
    )
    total_ms = timeline.total_duration_ms if timeline else None

    return RunTimeline(
        run=run_detail,
        iterations=iterations,
        total_duration_ms=total_ms,
    )


@router.get("/runs/compare", response_model=RunComparison)
def compare_two_runs(
    a: int = Query(..., description="First run ID"),
    b: int = Query(..., description="Second run ID"),
    session: Session = Depends(get_session),
) -> RunComparison:
    """Side-by-side comparison of two runs.

    Runs may belong to different conversations. Returns metric deltas and
    per-iteration timelines for both.
    """
    run_a = get_run(session, a)
    run_b = get_run(session, b)
    if run_a is None or run_b is None:
        raise HTTPException(status_code=404, detail="One or both runs not found")

    result = compare_runs(session, a, b)
    if result is None:
        raise HTTPException(status_code=404, detail="Comparison failed")

    return RunComparison(
        run_a=_run_to_out(run_a),
        run_b=_run_to_out(run_b),
        delta_tokens=result.delta_tokens,
        delta_cost_usd=result.delta_cost_usd,
        delta_iterations=result.delta_iterations,
        delta_duration_ms=result.delta_duration_ms,
        iterations_a=[_iteration_to_detail(i) for i in result.iterations_a],
        iterations_b=[_iteration_to_detail(i) for i in result.iterations_b],
    )


@router.post(
    "/conversations/{conv_id}/runs/{run_id}/replay", response_model=ReplayResponse
)
def replay_run(
    conv_id: int,
    run_id: int,
    body: ReplayRequest | None = None,
    session: Session = Depends(get_session),
) -> ReplayResponse:
    """Replay a run: re-execute the original input with optional overrides.

    Creates a new AgentRun in the same conversation. The actual execution
    happens when the client streams the conversation (SSE/WS) — this endpoint
    prepares the run row and re-inserts the original user message so the next
    stream picks it up.

    Overrides: model, system_prompt, temperature (all optional).
    """
    _require_run(session, conv_id, run_id)

    ctx = prepare_replay(
        session,
        run_id,
        model_override=body.model if body else None,
        system_prompt_override=body.system_prompt if body else None,
        temperature_override=body.temperature if body else None,
    )
    if ctx is None:
        raise HTTPException(status_code=404, detail="Cannot prepare replay")

    # Re-insert the original user message so the conversation has the input.
    if ctx.user_input:
        append_message(
            session,
            conversation_id=ctx.conversation_id,
            role="user",
            content=f"[Replay] {ctx.user_input}",
        )

    # Create the new run row (status=queued; the streaming endpoint will pick
    # it up and drive execution).
    new_run = create_run(
        session,
        conversation_id=ctx.conversation_id,
        model=ctx.model,
        config={
            "replay_of": run_id,
            "system_prompt_override": ctx.system_prompt,
            "temperature_override": ctx.temperature,
            "tool_names": ctx.tool_names,
        },
        status="queued",
    )

    return ReplayResponse(
        new_run_id=new_run.id,  # type: ignore[arg-type]
        original_run_id=run_id,
        status=new_run.status,
    )
