"""Subagent routes: role CRUD, launch/cancel runs, SSE streaming (Фаза 2 §5)."""

from __future__ import annotations

import asyncio
import json

from fastapi import APIRouter, Depends, HTTPException, Query, Request
from sqlmodel import Session
from sse_starlette.sse import EventSourceResponse

from app.agent.subagents import (
    cancel_subagent_run,
    create_role,
    delete_role,
    delete_subagent_run,
    get_role,
    get_subagent_messages,
    get_subagent_run,
    launch_subagent,
    list_roles,
    list_subagent_runs,
    update_role,
)
from app.api.schemas import (
    MessageOut,
    SubagentLaunchBatchRequest,
    SubagentLaunchRequest,
    SubagentRoleCreate,
    SubagentRoleOut,
    SubagentRoleUpdate,
    SubagentRunDetail,
    SubagentRunOut,
)
from app.core.db import get_session
from app.observability import inspector_registry

router = APIRouter()


def _role_to_out(role) -> SubagentRoleOut:
    return SubagentRoleOut(
        id=role.id,
        name=role.name,
        description=role.description,
        system_prompt=role.system_prompt,
        model=role.model,
        tool_names=role.tool_names,
        capability_policy=role.capability_policy,
        max_iterations=role.max_iterations,
        max_cost_usd=role.max_cost_usd,
        is_builtin=role.is_builtin,
        created_at=role.created_at,
        updated_at=role.updated_at,
    )


def _run_to_out(run) -> SubagentRunOut:
    return SubagentRunOut(
        id=run.id,
        role_id=run.role_id,
        parent_conversation_id=run.parent_conversation_id,
        parent_run_id=run.parent_run_id,
        conversation_id=run.conversation_id,
        run_id=run.run_id,
        name=run.name,
        prompt=run.prompt,
        status=run.status,
        result_summary=run.result_summary,
        usage=run.usage,
        error=run.error,
        started_at=run.started_at,
        finished_at=run.finished_at,
        created_at=run.created_at,
        updated_at=run.updated_at,
    )


def _msg_to_out(m) -> MessageOut:
    return MessageOut(
        id=m.id,
        conversation_id=m.conversation_id,
        role=m.role,
        content=m.content,
        artifact_ids=m.artifact_ids,
        tool_calls=m.tool_calls,
        usage=m.usage,
        thinking=m.thinking,
        tool_result=m.tool_result,
        created_at=m.created_at,
        model=m.model,
        duration_ms=m.duration_ms,
    )


# --- Role CRUD ---


@router.get("/subagents/tools", response_model=list[str])
def list_available_tools():
    """List the names of all registered tools (for the role editor picker)."""
    from app.tools import get_registry

    return sorted(get_registry().keys())


@router.get("/subagents/roles", response_model=list[SubagentRoleOut])
def list_subagent_roles(session: Session = Depends(get_session)):
    """List all subagent role definitions."""
    return [_role_to_out(r) for r in list_roles(session)]


@router.post("/subagents/roles", response_model=SubagentRoleOut, status_code=201)
def create_subagent_role(body: SubagentRoleCreate, session: Session = Depends(get_session)):
    """Create a new subagent role definition."""
    role = create_role(
        session,
        name=body.name,
        description=body.description,
        system_prompt=body.system_prompt,
        model=body.model,
        tool_names=body.tool_names,
        capability_policy=body.capability_policy,
        max_iterations=body.max_iterations,
        max_cost_usd=body.max_cost_usd,
    )
    return _role_to_out(role)


@router.get("/subagents/roles/{role_id}", response_model=SubagentRoleOut)
def get_subagent_role(role_id: int, session: Session = Depends(get_session)):
    """Get a single role definition."""
    role = get_role(session, role_id)
    if role is None:
        raise HTTPException(status_code=404, detail="Role not found")
    return _role_to_out(role)


@router.put("/subagents/roles/{role_id}", response_model=SubagentRoleOut)
def update_subagent_role(
    role_id: int, body: SubagentRoleUpdate, session: Session = Depends(get_session)
):
    """Update a role definition."""
    fields = body.model_dump(exclude_unset=True)
    role = update_role(session, role_id, **fields)
    if role is None:
        raise HTTPException(status_code=404, detail="Role not found")
    return _role_to_out(role)


@router.delete("/subagents/roles/{role_id}", status_code=204)
def delete_subagent_role(role_id: int, session: Session = Depends(get_session)):
    """Delete a non-builtin role definition."""
    role = get_role(session, role_id)
    if role is None:
        raise HTTPException(status_code=404, detail="Role not found")
    if role.is_builtin:
        raise HTTPException(status_code=403, detail="Cannot delete a built-in role")
    delete_role(session, role_id)


# --- Launch / Run management ---


@router.post("/subagents/launch", response_model=SubagentRunOut, status_code=201)
def launch_subagent_endpoint(body: SubagentLaunchRequest, session: Session = Depends(get_session)):
    """Launch a single subagent run."""
    role = None
    if body.role_id is not None:
        role = get_role(session, body.role_id)
        if role is None:
            raise HTTPException(status_code=404, detail="Role not found")

    sa_run = launch_subagent(
        session,
        prompt=body.prompt,
        parent_conversation_id=body.parent_conversation_id,
        role=role,
        name=body.name,
        model_override=body.model,
    )
    return _run_to_out(sa_run)


@router.post("/subagents/launch-batch", response_model=list[SubagentRunOut], status_code=201)
def launch_subagent_batch(
    body: SubagentLaunchBatchRequest, session: Session = Depends(get_session)
):
    """Launch multiple subagents simultaneously."""
    results = []
    for item in body.items:
        role = None
        if item.role_id is not None:
            role = get_role(session, item.role_id)
            if role is None:
                raise HTTPException(status_code=404, detail=f"Role {item.role_id} not found")
        sa_run = launch_subagent(
            session,
            prompt=item.prompt,
            parent_conversation_id=body.parent_conversation_id,
            role=role,
            name=item.name,
            model_override=item.model,
        )
        results.append(_run_to_out(sa_run))
    return results


@router.get("/subagents/runs", response_model=list[SubagentRunOut])
def list_subagent_runs_endpoint(
    parent_conversation_id: int | None = Query(default=None),
    status: str | None = Query(default=None),
    session: Session = Depends(get_session),
):
    """List subagent runs, optionally filtered."""
    runs = list_subagent_runs(session, parent_conversation_id=parent_conversation_id, status=status)
    return [_run_to_out(r) for r in runs]


@router.get("/subagents/runs/{run_id}", response_model=SubagentRunDetail)
def get_subagent_run_endpoint(run_id: int, session: Session = Depends(get_session)):
    """Get a subagent run with its conversation messages."""
    sa_run = get_subagent_run(session, run_id)
    if sa_run is None:
        raise HTTPException(status_code=404, detail="Subagent run not found")
    messages = get_subagent_messages(session, run_id)
    out = _run_to_out(sa_run)
    return SubagentRunDetail(**out.model_dump(), messages=[_msg_to_out(m) for m in messages])


@router.post("/subagents/runs/{run_id}/cancel")
def cancel_subagent_endpoint(run_id: int, session: Session = Depends(get_session)):
    """Cancel a running subagent."""
    sa_run = get_subagent_run(session, run_id)
    if sa_run is None:
        raise HTTPException(status_code=404, detail="Subagent run not found")
    cancelled = cancel_subagent_run(session, run_id)
    return {"run_id": run_id, "cancelled": cancelled}


@router.delete("/subagents/runs/{run_id}", status_code=204)
def delete_subagent_run_endpoint(run_id: int, session: Session = Depends(get_session)):
    """Delete a completed/failed/cancelled subagent run record."""
    sa_run = get_subagent_run(session, run_id)
    if sa_run is None:
        raise HTTPException(status_code=404, detail="Subagent run not found")
    if not delete_subagent_run(session, run_id):
        raise HTTPException(status_code=409, detail="Cannot delete a non-terminal run")


@router.get("/subagents/runs/{run_id}/stream")
async def stream_subagent_run(
    run_id: int, request: Request, session: Session = Depends(get_session)
):
    """SSE stream of live events for a subagent run (via inspector registry)."""
    sa_run = get_subagent_run(session, run_id)
    if sa_run is None:
        raise HTTPException(status_code=404, detail="Subagent run not found")
    if sa_run.run_id is None:
        raise HTTPException(status_code=400, detail="Run has no associated agent run")

    agent_run_id = sa_run.run_id

    async def event_generator():
        queue = inspector_registry.subscribe(agent_run_id)
        try:
            while True:
                if await request.is_disconnected():
                    break
                try:
                    event = await asyncio.wait_for(queue.get(), timeout=30.0)
                except TimeoutError:
                    # Send keepalive.
                    yield {"event": "keepalive", "data": "{}"}
                    continue
                if event is None:
                    # Run finished sentinel.
                    break
                yield {
                    "event": event.get("kind", "message"),
                    "data": json.dumps(event, default=str, ensure_ascii=False),
                }
        finally:
            inspector_registry.unsubscribe(agent_run_id, queue)

    return EventSourceResponse(event_generator())
