"""Agent run routes: list/detail runs, their event log, and cancellation.

A run is created implicitly when a message is sent (see the SSE/WS handlers);
these endpoints let the client observe and control runs:

  GET    /conversations/{conv}/runs               — list runs for a conversation
  GET    /conversations/{conv}/runs/{run}         — run detail + event log
  GET    /conversations/{conv}/runs/{run}/events  — event log only
  POST   /conversations/{conv}/runs/{run}/cancel  — signal cancellation
"""

from __future__ import annotations

from fastapi import APIRouter, Depends, HTTPException
from sqlmodel import Session

from app.agent.runs import run_registry
from app.agent.service import get_conversation, get_run, list_run_events, list_runs
from app.api.schemas import CancelRunResponse, RunDetail, RunEventOut, RunOut
from app.core.db import get_session

router = APIRouter()


def _require_conversation(session: Session, conv_id: int) -> None:
    """404 if the conversation doesn't exist (runs are scoped to one)."""
    if get_conversation(session, conv_id) is None:
        raise HTTPException(status_code=404, detail="Conversation not found")


def _require_run(session: Session, conv_id: int, run_id: int):
    """Load a run, 404 if missing or not belonging to this conversation."""
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


@router.get("/conversations/{conv_id}/runs", response_model=list[RunOut])
def list_conversation_runs(conv_id: int, session: Session = Depends(get_session)) -> list[RunOut]:
    """List runs for a conversation, newest first."""
    _require_conversation(session, conv_id)
    return [_run_to_out(r) for r in list_runs(session, conversation_id=conv_id)]


@router.get("/conversations/{conv_id}/runs/{run_id}", response_model=RunDetail)
def get_run_detail(conv_id: int, run_id: int, session: Session = Depends(get_session)) -> RunDetail:
    """Run detail including config snapshot, checkpoint, and full event log."""
    run = _require_run(session, conv_id, run_id)
    events = [_event_to_out(e) for e in list_run_events(session, run_id=run_id)]
    out = _run_to_out(run)
    return RunDetail(
        **out.model_dump(),
        config=run.config,
        checkpoint=run.checkpoint,
        events=events,
    )


@router.get("/conversations/{conv_id}/runs/{run_id}/events", response_model=list[RunEventOut])
def get_run_events(
    conv_id: int, run_id: int, session: Session = Depends(get_session)
) -> list[RunEventOut]:
    """A run's append-only event log, in the order the loop emitted it."""
    _require_run(session, conv_id, run_id)
    return [_event_to_out(e) for e in list_run_events(session, run_id=run_id)]


@router.post("/conversations/{conv_id}/runs/{run_id}/cancel", response_model=CancelRunResponse)
def cancel_run(
    conv_id: int, run_id: int, session: Session = Depends(get_session)
) -> CancelRunResponse:
    """Signal a running (cancellable) run to stop.

    Returns ``cancelled: false`` if the run is unknown or already finished (the
    registry only holds active runs). Cancelling a completed/failed run is a
    no-op, not an error, so a client can fire-and-forget without racing the run
    lifecycle.
    """
    _require_run(session, conv_id, run_id)
    cancelled = run_registry.cancel(run_id)
    return CancelRunResponse(run_id=run_id, cancelled=cancelled)
