"""Deep Research API: start/list/detail/cancel/rerun/export + SSE stream.

``POST /api/research/stream`` drives the pipeline inline and yields progress
events as an SSE stream (stage, subquestion_*, source_found, completed/failed/
cancelled). ``POST /api/research`` schedules a background run (used by cron
integration and tool-triggered research). Export serves the report as markdown
or self-contained HTML from the stored artifact.
"""

from __future__ import annotations

import asyncio
import contextlib
import html
import json
import re
from collections.abc import AsyncIterator

from fastapi import APIRouter, Depends, HTTPException
from pydantic import BaseModel
from sqlmodel import Session
from sse_starlette.sse import EventSourceResponse

from app.api.schemas import (
    ResearchCitationOut,
    ResearchRerunRequest,
    ResearchRunDetail,
    ResearchRunOut,
    ResearchSourceOut,
    ResearchStartRequest,
)
from app.core.db import get_session
from app.research import (
    cancel_research_run,
    get_research_run,
    list_research_runs,
    rerun_research,
    research_registry,
    start_research,
)
from app.research.orchestrator import EventSink, execute_research

router = APIRouter()


def _run_to_out(run) -> ResearchRunOut:
    return ResearchRunOut(
        id=run.id,
        topic=run.topic,
        depth=run.depth,
        model=run.model,
        status=run.status,
        input_hash=run.input_hash,
        report_artifact_id=run.report_artifact_id,
        sources_count=len(run.sources or []),
        citations_count=len(run.citations or []),
        usage=run.usage,
        error=run.error,
        created_at=run.created_at,
        updated_at=run.updated_at,
        finished_at=run.finished_at,
    )


def _run_to_detail(run) -> ResearchRunDetail:
    detail = _run_to_out(run)
    return ResearchRunDetail(
        **detail.model_dump(),
        conversation_id=run.conversation_id,
        parent_task_run_id=run.parent_task_run_id,
        sub_questions=run.sub_questions or [],
        sources=[ResearchSourceOut(**s) for s in (run.sources or [])],
        citations=[ResearchCitationOut(**c) for c in (run.citations or [])],
        report_markdown=run.report_markdown,
    )


def _launch_background(run_id: int) -> None:
    """Schedule execute_research as an asyncio task when a loop is running."""
    try:
        loop = asyncio.get_running_loop()
        task = loop.create_task(execute_research(run_id))
        research_registry.register(run_id, task)
    except RuntimeError:
        pass  # No running loop (sync test client) — stays queued.


# --- CRUD ------------------------------------------------------------------


@router.get("/research", response_model=list[ResearchRunOut])
def get_research_runs(
    limit: int = 50, session: Session = Depends(get_session)
) -> list[ResearchRunOut]:
    """List research runs, newest first."""
    return [_run_to_out(r) for r in list_research_runs(session, limit=min(limit, 200))]


@router.get("/research/{run_id}", response_model=ResearchRunDetail)
def get_research_run_detail(
    run_id: int, session: Session = Depends(get_session)
) -> ResearchRunDetail:
    """Full detail: sub-questions, sources, citations, report markdown."""
    run = get_research_run(session, run_id)
    if run is None:
        raise HTTPException(status_code=404, detail="Research run not found")
    return _run_to_detail(run)


@router.post("/research", response_model=ResearchRunOut, status_code=201)
def post_research(
    body: ResearchStartRequest, session: Session = Depends(get_session)
) -> ResearchRunOut:
    """Create a research run and execute it in the background."""
    topic = body.topic.strip()
    if not topic:
        raise HTTPException(status_code=400, detail="Topic is required")
    depth = min(max(body.depth, 3), 5)
    run = start_research(
        session,
        topic=topic,
        depth=depth,
        model=body.model,
        conversation_id=body.conversation_id,
    )
    _launch_background(run.id)
    return _run_to_out(run)


@router.post("/research/stream")
async def stream_research(
    body: ResearchStartRequest, session: Session = Depends(get_session)
) -> EventSourceResponse:
    """Start research and stream progress events as SSE until completion.

    Events are ``{"type": ..., "payload": {...}}`` dicts: stage,
    subquestion_started / subquestion_completed, source_found, completed /
    failed / cancelled. The run row stays visible via GET /api/research.
    """
    topic = body.topic.strip()
    if not topic:
        raise HTTPException(status_code=400, detail="Topic is required")
    depth = min(max(body.depth, 3), 5)
    run = start_research(
        session,
        topic=topic,
        depth=depth,
        model=body.model,
        conversation_id=body.conversation_id,
    )
    run_id = run.id

    queue: asyncio.Queue[dict] = asyncio.Queue()
    sink = EventSink.for_queue(queue)
    # Announce the run id up front so the client can render progress and cancel.
    await queue.put({"type": "started", "payload": {"run_id": run_id}})

    async def _drive() -> None:
        await execute_research(run_id, sink=sink)

    task = asyncio.create_task(_drive())

    async def event_stream() -> AsyncIterator[dict]:
        try:
            while True:
                # Race the next queued event against task completion so a
                # pipeline crash outside execute_research still yields a
                # terminal event instead of hanging the stream.
                get_task = asyncio.create_task(queue.get())
                done, _ = await asyncio.wait(
                    {get_task, task}, return_when=asyncio.FIRST_COMPLETED
                )
                if get_task not in done:
                    exc = task.exception()
                    if exc is not None:
                        yield {
                            "event": "research",
                            "data": json.dumps(
                                {"type": "failed", "payload": {"error": str(exc)}}
                            ),
                        }
                    break
                event = get_task.result()
                yield {"event": "research", "data": json.dumps(event)}
                if event["type"] in ("completed", "failed", "cancelled"):
                    break
        finally:
            if not task.done():
                task.cancel()
            with contextlib.suppress(asyncio.CancelledError):
                await task

    return EventSourceResponse(event_stream())


@router.post("/research/{run_id}/cancel", response_model=dict)
def cancel_research(run_id: int, session: Session = Depends(get_session)) -> dict:
    """Cancel a running research workflow."""
    if not cancel_research_run(session, run_id):
        raise HTTPException(status_code=404, detail="Research run not found or already finished")
    return {"cancelled": run_id}


@router.post("/research/{run_id}/rerun", response_model=ResearchRunOut, status_code=201)
def rerun_research_endpoint(
    run_id: int, body: ResearchRerunRequest, session: Session = Depends(get_session)
) -> ResearchRunOut:
    """Repeat a previous run with the same inputs (optionally a new model)."""
    if get_research_run(session, run_id) is None:
        raise HTTPException(status_code=404, detail="Research run not found")
    run = rerun_research(session, run_id=run_id, model=body.model)
    _launch_background(run.id)
    return _run_to_out(run)


# --- Export ----------------------------------------------------------------


class ExportFormat(BaseModel):
    format: str = "md"


@router.get("/research/{run_id}/export")
def export_research(
    run_id: int,
    format: str = "md",
    session: Session = Depends(get_session),
):
    """Export the report as markdown or self-contained HTML."""
    run = get_research_run(session, run_id)
    if run is None or not run.report_markdown:
        raise HTTPException(status_code=404, detail="Report not available")
    if format == "html":
        content = _md_to_html(run.report_markdown)
        media = "text/html; charset=utf-8"
        filename = f"research-{run.id}.html"
    else:
        content = run.report_markdown
        media = "text/markdown; charset=utf-8"
        filename = f"research-{run.id}.md"
    from fastapi.responses import Response

    return Response(
        content=content,
        media_type=media,
        headers={"Content-Disposition": f'attachment; filename="{filename}"'},
    )


def _md_to_html(markdown: str) -> str:
    """Minimal markdown → HTML for export (headings, lists, bold, code, links)."""
    lines: list[str] = []
    in_list = False
    in_code = False
    code_buf: list[str] = []
    for raw in markdown.splitlines():
        line = raw.rstrip()
        if line.strip().startswith("```"):
            if in_code:
                lines.append("<pre>" + html.escape("\n".join(code_buf)) + "</pre>")
                code_buf = []
                in_code = False
            else:
                in_code = True
            continue
        if in_code:
            code_buf.append(line)
            continue
        if line.strip().startswith("|"):
            continue  # skip tables
        heading = re.match(r"^(#{1,6})\s+(.*)$", line)
        if heading:
            level = len(heading.group(1))
            lines.append(f"<h{level}>{html.escape(heading.group(2))}</h{level}>")
            continue
        bullet = re.match(r"^[-*]\s+(.*)$", line)
        if bullet:
            if not in_list:
                lines.append("<ul>")
                in_list = True
            lines.append(f"<li>{_inline_md(bullet.group(1))}</li>")
            continue
        if in_list and line.strip():
            lines.append("</ul>")
            in_list = False
        if line.strip():
            lines.append(f"<p>{_inline_md(line)}</p>")
    if in_list:
        lines.append("</ul>")
    body = "\n".join(lines)
    return (
        '<!DOCTYPE html><html><head><meta charset="utf-8">'
        "<title>Research Report</title>"
        "<style>body{font-family:system-ui,sans-serif;max-width:860px;"
        "margin:2rem auto;padding:0 1rem;line-height:1.6}h1,h2{border-bottom:"
        "1px solid #e5e7eb;padding-bottom:.3rem}a{color:#2563eb}</style>"
        "</head><body>" + body + "</body></html>"
    )


def _inline_md(text: str) -> str:
    """Inline markdown: bold, italics, inline code, links (escaped)."""
    escaped = html.escape(text)
    escaped = re.sub(r"\[([^\]]+)\]\((https?://[^)]+)\)", r'<a href="\2">\1</a>', escaped)
    escaped = re.sub(r"\*\*([^*]+)\*\*", r"<strong>\1</strong>", escaped)
    escaped = re.sub(r"`([^`]+)`", r"<code>\1</code>", escaped)
    return escaped
