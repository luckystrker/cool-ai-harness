"""Deep Research workflow orchestrator (Фаза 4 — Deep Research).

Pipeline for one ``ResearchRun``:

1. **decompose** — one LLM call splits the topic into ``depth`` sub-questions.
2. **gather** — researcher subagents (existing subagent infrastructure) run in
   parallel (bounded by a semaphore); each searches the web and fetches pages
   for one sub-question, returning findings with source URLs.
3. **collect** — subagent outputs are parsed into structured sources
   (``{url, title, snippet, fetched_at, confidence, conflict}``).
4. **synthesize** — one LLM call writes a markdown report with inline ``[n]``
   citations, a bibliography, confidence annotations and conflict flags.
5. **persist** — the report is stored as an ``Artifact`` (kind=report) and the
   run row is finalized with sources/citations/usage.

Progress is reported through an optional ``EventSink`` (an asyncio queue the
SSE endpoint drains); background execution (tool, cron) passes ``None`` and
still gets a fully durable run row.

Cancellation: the owning asyncio task is tracked in ``research_registry``;
cancelling it propagates into the in-flight subagent tasks, which mark
themselves cancelled, and the run row is finalized as ``cancelled``.
"""

from __future__ import annotations

import asyncio
import hashlib
import json
import re
from datetime import UTC, datetime
from typing import Any

from sqlmodel import Session, select

from app.agent.service import (
    create_conversation,
    get_or_create_default_user,
    resolve_default_model,
)
from app.agent.subagents import create_subagent_run, execute_subagent, get_role_by_name
from app.artifacts import store_artifact
from app.core.db import engine
from app.core.logging import get_logger
from app.models.research import (
    RESEARCH_MAX_CONCURRENT_SUBAGENTS,
    RESEARCH_STATUS_CANCELLED,
    RESEARCH_STATUS_COMPLETED,
    RESEARCH_STATUS_FAILED,
    RESEARCH_STATUS_QUEUED,
    RESEARCH_STATUS_RUNNING,
    ResearchRun,
)
from app.providers import Message, get_provider_for_model

log = get_logger(__name__)

# A research report may be long; cap what tools/tasks put inline.
MAX_REPORT_OUTPUT_CHARS = 20_000

_URL_RE = re.compile(r"https?://[^\s)\]}>\"']+")
_NUMBERED_LINE_RE = re.compile(r"^\s*\d+[.)]\s+(.+)$")
_CITE_JSON_MARKER = "CITE_JSON:"
_SENT_BOUNDARY_RE = re.compile(r"[.!?]\s|\n")

MAX_SOURCES_PER_SUBAGENT = 8
MAX_TOTAL_SOURCES = 40
MAX_SNIPPET_CHARS = 400


class EventSink:
    """Pushes research progress events to an asyncio queue (SSE transport).

    Pass ``None`` for background execution (tool / cron) to drop events.
    """

    def __init__(self, queue: asyncio.Queue[dict[str, Any]] | None = None) -> None:
        self.queue = queue

    async def emit(self, type_: str, **payload: Any) -> None:
        if self.queue is not None:
            await self.queue.put({"type": type_, "payload": payload})

    @classmethod
    def for_queue(cls, queue: asyncio.Queue[dict[str, Any]]) -> EventSink:
        return cls(queue)


# --- Run lifecycle ---------------------------------------------------------


def _input_hash(topic: str, depth: int, model: str | None) -> str:
    """Stable digest of the workflow inputs — groups runs for repeat/compare."""
    return hashlib.sha256(f"{topic}|{depth}|{model or ''}".encode()).hexdigest()


def start_research(
    session: Session,
    *,
    topic: str,
    depth: int = 4,
    model: str | None = None,
    conversation_id: int | None = None,
    parent_task_run_id: int | None = None,
) -> ResearchRun:
    """Create the durable ResearchRun row (status=queued).

    When ``conversation_id`` is None a hidden research conversation is created:
    it hosts the researcher subagents and the report artifact, keeping the
    artifact library intact for standalone research.
    """
    user = get_or_create_default_user(session)

    if conversation_id is None:
        conv = create_conversation(
            session,
            user_id=user.id,
            title=f"[Research] {topic[:60]}",
            model=model or resolve_default_model(session),
        )
        conv.metadata_ = {**(conv.metadata_ or {}), "is_research": True}
        session.add(conv)
        session.commit()
        session.refresh(conv)
        research_conversation_id = conv.id
    else:
        research_conversation_id = conversation_id

    run = ResearchRun(
        user_id=user.id,
        conversation_id=research_conversation_id,
        parent_task_run_id=parent_task_run_id,
        topic=topic,
        depth=depth,
        model=model,
        status=RESEARCH_STATUS_QUEUED,
        input_hash=_input_hash(topic, depth, model),
    )
    session.add(run)
    session.commit()
    session.refresh(run)
    log.info("research.started", research_run_id=run.id, topic=topic[:80], depth=depth)
    return run


def get_research_run(session: Session, run_id: int) -> ResearchRun | None:
    return session.get(ResearchRun, run_id)


def list_research_runs(session: Session, *, limit: int = 50) -> list[ResearchRun]:
    return list(
        session.exec(select(ResearchRun).order_by(ResearchRun.id.desc()).limit(limit)).all()
    )


def cancel_research_run(session: Session, run_id: int) -> bool:
    """Cancel a running research workflow. Returns True if cancelled."""
    from app.research.registry import research_registry

    run = session.get(ResearchRun, run_id)
    if run is None or run.status in (
        RESEARCH_STATUS_COMPLETED,
        RESEARCH_STATUS_FAILED,
        RESEARCH_STATUS_CANCELLED,
    ):
        return False
    cancelled = research_registry.cancel(run_id)
    if not cancelled:
        run.status = RESEARCH_STATUS_CANCELLED
        run.finished_at = datetime.now(UTC)
        session.add(run)
        session.commit()
    return True


def rerun_research(
    session: Session,
    *,
    run_id: int,
    model: str | None = None,
) -> ResearchRun:
    """Repeat a research run with the same inputs (optionally a new model)."""
    original = session.get(ResearchRun, run_id)
    if original is None:
        raise LookupError(f"Research run {run_id} not found")
    return start_research(
        session,
        topic=original.topic,
        depth=original.depth,
        model=model or original.model,
        conversation_id=original.conversation_id,
    )


def _launch_task(run_id: int) -> None:
    """Schedule execute_research as a background asyncio task when possible."""
    from app.research.registry import research_registry

    try:
        loop = asyncio.get_running_loop()
        task = loop.create_task(execute_research(run_id))
        research_registry.register(run_id, task)
    except RuntimeError:
        # No running event loop (sync test client) — stays queued.
        log.debug("research.no_loop", research_run_id=run_id)


# --- Orchestration ---------------------------------------------------------


async def execute_research(
    research_run_id: int,
    sink: EventSink | None = None,
) -> str | None:
    """Run the research pipeline to completion. Returns the report markdown.

    Designed to run as an asyncio task. Opens its own DB session, drives the
    stages, and finalizes the run row (completed/failed/cancelled).
    """
    sink = sink or EventSink(None)
    with Session(engine) as session:
        run = session.get(ResearchRun, research_run_id)
        if run is None:
            log.error("research.not_found", research_run_id=research_run_id)
            return None

        model = run.model or resolve_default_model(session)
        if model is None:
            _fail_run(session, run, "No model configured for research")
            return None

        run.status = RESEARCH_STATUS_RUNNING
        session.add(run)
        session.commit()

        usage_total = {
            "prompt_tokens": 0,
            "completion_tokens": 0,
            "total_tokens": 0,
            "cost_usd": 0.0,
        }
        try:
            # Resolved inside the try so provider failures mark the run failed
            # (and the SSE stream gets a terminal event) instead of escaping.
            provider = get_provider_for_model(model)

            # --- Stage 1: decompose ---------------------------------------
            await sink.emit("stage", stage="decompose")
            sub_questions, decompose_usage = await _decompose(
                provider, model, run.topic, run.depth
            )
            _accumulate_usage(usage_total, decompose_usage)
            if not sub_questions:
                raise RuntimeError("Topic decomposition returned no sub-questions")
            run.sub_questions = sub_questions
            session.add(run)
            session.commit()
            log.info("research.decomposed", research_run_id=run.id, count=len(sub_questions))

            # --- Stage 2: gather (parallel subagents) ---------------------
            await sink.emit("stage", stage="gather")
            findings = await _gather(
                session,
                sink=sink,
                topic=run.topic,
                sub_questions=sub_questions,
                conversation_id=run.conversation_id,
                model=model,
            )

            # --- Stage 3: collect sources ---------------------------------
            sources: list[dict[str, Any]] = []
            for question, text in zip(sub_questions, findings, strict=False):
                if not text:
                    continue
                extracted = _extract_sources(text, sub_question=question)
                sources.extend(extracted)
            # De-duplicate by URL across subagents.
            sources = _dedupe_sources(sources)
            run.sources = sources
            session.add(run)
            session.commit()
            log.info("research.sources", research_run_id=run.id, count=len(sources))
            for src in sources:
                await sink.emit("source_found", url=src["url"], title=src["title"])

            # --- Stage 4: synthesize --------------------------------------
            await sink.emit("stage", stage="synthesize")
            report, citations, synth_usage = await _synthesize(
                provider, model, run.topic, sub_questions, sources
            )
            report = (report or "").strip()
            if not report:
                raise RuntimeError("Synthesis returned an empty report")

            # --- Stage 5: persist -----------------------------------------
            artifact = store_artifact(
                session,
                conversation_id=run.conversation_id or 0,
                filename=f"research-{run.id}.md",
                content=report.encode("utf-8"),
                kind="report",
                media_type="text/markdown",
                metadata={
                    "research_run_id": run.id,
                    "topic": run.topic,
                    "model": model,
                },
            )
            run.report_markdown = report
            run.citations = citations
            run.report_artifact_id = artifact.id
            _accumulate_usage(usage_total, synth_usage)
            run.usage = usage_total
            run.status = RESEARCH_STATUS_COMPLETED
            run.finished_at = datetime.now(UTC)
            session.add(run)
            session.commit()
            log.info(
                "research.completed",
                research_run_id=run.id,
                sources=len(sources),
                citations=len(citations),
            )
            await sink.emit(
                "completed",
                run_id=run.id,
                report_artifact_id=artifact.id,
                sources_count=len(sources),
                citations_count=len(citations),
            )
            return report

        except asyncio.CancelledError:
            run.status = RESEARCH_STATUS_CANCELLED
            run.finished_at = datetime.now(UTC)
            session.add(run)
            session.commit()
            log.info("research.cancelled", research_run_id=run.id)
            await sink.emit("cancelled", run_id=run.id)
            raise

        except Exception as exc:
            _fail_run(session, run, str(exc))
            await sink.emit("failed", run_id=run.id, error=str(exc))
            return None

        finally:
            from app.research.registry import research_registry

            research_registry.unregister(research_run_id)


def _fail_run(session: Session, run: ResearchRun, error: str) -> None:
    run.status = RESEARCH_STATUS_FAILED
    run.error = error
    run.finished_at = datetime.now(UTC)
    session.add(run)
    session.commit()
    log.error("research.failed", research_run_id=run.id, error=error)


def _accumulate_usage(total: dict[str, Any], usage: Any | None) -> None:
    if usage is None:
        return
    for key in ("prompt_tokens", "completion_tokens", "total_tokens"):
        value = getattr(usage, key, 0)
        total[key] = int(total.get(key, 0) or 0) + int(value or 0)
    cost = getattr(usage, "cost_usd", None)
    if cost is not None:
        total["cost_usd"] = float(total.get("cost_usd", 0.0) or 0.0) + float(cost or 0.0)


# --- Stage 1: decomposition ------------------------------------------------


async def _decompose(
    provider: Any, model: str, topic: str, depth: int
) -> tuple[list[str], Any]:
    """Ask the LLM to split the topic into ``depth`` sub-questions.

    Returns (questions, usage).
    """
    prompt = (
        "Decompose the following research topic into exactly "
        f"{depth} distinct, focused research questions.\n\n"
        "Topic: {topic}\n\n"
        "Each question must target a separate aspect of the topic so the "
        "answers together cover it fully. Return ONLY a numbered list, one "
        "question per line, in the format: 1. question text"
    ).format(topic=topic)
    result = await provider.chat_completion(
        [Message(role="user", content=prompt)],
        model=model,
        temperature=0.4,
        max_tokens=1000,
    )
    questions: list[str] = []
    for line in (result.content or "").splitlines():
        match = _NUMBERED_LINE_RE.match(line)
        if match:
            question = match.group(1).strip()
            if question:
                questions.append(question)
    return questions[:depth], result.usage


# --- Stage 2: gathering ----------------------------------------------------


async def _gather(
    session: Session,
    *,
    sink: EventSink,
    topic: str,
    sub_questions: list[str],
    conversation_id: int | None,
    model: str | None,
) -> list[str]:
    """Run one researcher subagent per sub-question (bounded concurrency)."""
    semaphore = asyncio.Semaphore(RESEARCH_MAX_CONCURRENT_SUBAGENTS)

    async def _run_one(index: int, question: str) -> str | None:
        async with semaphore:
            await sink.emit("subquestion_started", index=index, sub_question=question)
            try:
                result = await _run_researcher_subagent(
                    session,
                    topic=topic,
                    sub_question=question,
                    index=index,
                    total=len(sub_questions),
                    conversation_id=conversation_id,
                    model=model,
                )
                await sink.emit(
                    "subquestion_completed",
                    index=index,
                    status="completed" if result else "empty",
                )
                return result or ""
            except asyncio.CancelledError:
                raise
            except Exception as exc:
                log.warning(
                    "research.subagent_failed",
                    index=index,
                    error=str(exc),
                )
                await sink.emit("subquestion_completed", index=index, status="failed")
                return ""

    results: list[str | None] = await asyncio.gather(
        *(_run_one(i, q) for i, q in enumerate(sub_questions))
    )
    return [r or "" for r in results]


async def _run_researcher_subagent(
    session: Session,
    *,
    topic: str,
    sub_question: str,
    index: int,
    total: int,
    conversation_id: int | None,
    model: str | None,
) -> str | None:
    """Spawn + await one researcher subagent; returns its findings text."""
    prompt = (
        f"Research question: {sub_question}\n\n"
        f"Context: this is sub-question {index + 1} of {total} for the topic:\n"
        f"{topic}\n\n"
        "Use web_search to find relevant sources and web_fetch to read the "
        "most promising ones. Then write your findings:\n"
        "- Present 3-8 distinct findings as numbered claims.\n"
        "- Immediately after each claim, put the supporting URL(s) in "
        "[brackets].\n"
        "- End with a '## Sources' section listing every URL you used, one per "
        "line, in the format: URL | Title of the page\n"
        "Keep the whole response under 1500 words. Report facts only; note "
        "explicitly when information is uncertain."
    )

    role = get_role_by_name(session, "researcher")
    sa_run = create_subagent_run(
        session,
        prompt=prompt,
        parent_conversation_id=conversation_id or 1,
        role=role,
        model_override=model,
    )
    try:
        return await execute_subagent(sa_run.id)
    except asyncio.CancelledError:
        raise
    except Exception as exc:
        session.refresh(sa_run)
        if sa_run.error:
            raise RuntimeError(f"Researcher subagent error: {sa_run.error}") from exc
        raise


# --- Stage 3: source extraction -------------------------------------------


def _extract_sources(findings: str, *, sub_question: str) -> list[dict[str, Any]]:
    """Parse a subagent's findings text into structured sources.

    URLs are located anywhere in the text; titles are looked up in the
    ``## Sources`` section (``URL | Title`` lines); snippets are the sentence
    around each URL.
    """
    now = datetime.now(UTC).isoformat()
    seen: set[str] = set()
    sources: list[dict[str, Any]] = []
    for match in _URL_RE.finditer(findings):
        url = match.group(0).rstrip(".,;:)]")
        if url in seen:
            continue
        seen.add(url)
        title = _lookup_title(findings, url)
        snippet = _snippet_around(findings, match.start())
        sources.append(
            {
                "url": url,
                "title": title,
                "snippet": snippet,
                "fetched_at": now,
                "sub_question": sub_question,
                "confidence": "high" if title else "medium",
                "conflict": False,
            }
        )
        if len(sources) >= MAX_SOURCES_PER_SUBAGENT:
            break
    return sources


_TITLE_LINE_RE = re.compile(
    r"^\s*(?:[-*]\s*)?(?:\[)?(?:https?://[^\s)\]>\"']+|.*?)\s*[)\]|]\s*[-—|]\s*(.+?)\s*$"
)


def _lookup_title(findings: str, url: str) -> str:
    """Try to find ``URL | Title`` (or ``URL - Title``) in the Sources section."""
    sources_section = ""
    for marker in ("## Sources", "## Sources:", "# Sources", "Sources:"):
        idx = findings.find(marker)
        if idx != -1:
            sources_section = findings[idx:]
            break
    for line in sources_section.splitlines():
        if url in line:
            # Format: URL | Title
            if "|" in line:
                title = line.split("|", 1)[1].strip()
                if title and "|" not in title:
                    return title[:200]
            # Format: URL - Title / URL — Title
            for sep in (" — ", " - ", " | "):
                if sep in line:
                    title = line.split(sep, 1)[1].strip()
                    if title:
                        return title[:200]
            # Markdown link [Title](url)
            link_match = re.search(r"\[([^\]]+)\]\(([^)]+)\)", line)
            if link_match and url in link_match.group(2):
                return link_match.group(1).strip()[:200]
            return ""
    return ""


def _snippet_around(text: str, position: int) -> str:
    """A readable snippet around ``position``, trimmed to sentence bounds."""
    start = max(0, position - 140)
    end = min(len(text), position + 260)
    snippet = text[start:end].replace("\n", " ").strip()
    if start > 0:
        snippet = "…" + snippet
    if end < len(text):
        snippet = snippet + "…"
    return snippet[:MAX_SNIPPET_CHARS]


def _dedupe_sources(sources: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """Keep one entry per URL; merge sub-question tags for duplicates."""
    by_url: dict[str, dict[str, Any]] = {}
    for src in sources:
        existing = by_url.get(src["url"])
        if existing is None:
            by_url[src["url"]] = dict(src)
            continue
        # Keep the richer entry; add the sub-question to the tag list.
        if src["title"] and not existing.get("title"):
            existing["title"] = src["title"]
        questions = existing.get("sub_questions") or [existing.get("sub_question")]
        if src.get("sub_question") and src["sub_question"] not in questions:
            questions.append(src["sub_question"])
        existing["sub_questions"] = questions
    deduped = list(by_url.values())
    return deduped[:MAX_TOTAL_SOURCES]


# --- Stage 4: synthesis ----------------------------------------------------


async def _synthesize(
    provider: Any,
    model: str,
    topic: str,
    sub_questions: list[str],
    sources: list[dict[str, Any]],
) -> tuple[str, list[dict[str, Any]], Any]:
    """Write the final report with inline [n] citations + bibliography."""
    source_lines = [
        f"[{i + 1}] {s['url']} — {s['title'] or '(no title)'}" for i, s in enumerate(sources)
    ]
    prompt = (
        "You are a research analyst. Below are findings gathered from "
        f"{len(sources)} web sources about the topic: {topic}\n\n"
        "SOURCES:\n" + "\n".join(source_lines) + "\n\n"
        "Sub-questions to answer in the analysis:\n"
        + "\n".join(f"- {q}" for q in sub_questions)
        + "\n\n"
        "Write a structured markdown report:\n"
        f"# {topic}\n"
        "## Executive Summary\n(2-3 sentences)\n"
        "## Key Findings\n(numbered, most important first)\n"
        "## Detailed Analysis\n(one subsection per sub-question, with evidence)\n"
        "## Limitations & Gaps\n(what could not be verified)\n"
        "## Sources\n(the numbered URL list, always the final section)\n\n"
        "CITING: every factual claim must be followed by its source number(s) "
        "like [1] or [2][3]. Every source in ## Sources must be cited at least "
        "once. If sources disagree, say so explicitly and label it a conflict.\n"
        "After the report, output a single line starting with "
        "CITE_JSON: followed by a JSON object: "
        '{"citations": [{"index": 1, "confidence": "high|medium|low", '
        '"conflict": false}]} — one entry per citation index used in the report.'
    )
    result = await provider.chat_completion(
        [Message(role="user", content=prompt)],
        model=model,
        temperature=0.3,
        max_tokens=4000,
    )
    text = result.content or ""
    citations = _extract_citations(text, len(sources))
    return text, citations, result.usage


def _extract_citations(report: str, source_count: int) -> list[dict[str, Any]]:
    """Parse ``[n]`` markers and the trailing CITE_JSON block into citations.

    Each citation carries the claim sentence (for clickable in-text markers),
    the referenced source index, and confidence/conflict from the model.
    """
    # Confidence/conflict map from the trailing CITE_JSON block.
    annotations: dict[int, dict[str, Any]] = {}
    json_start = report.find(_CITE_JSON_MARKER)
    if json_start != -1:
        raw = report[json_start + len(_CITE_JSON_MARKER) :].strip()
        try:
            parsed = json.loads(raw)
            for entry in parsed.get("citations", []):
                idx = int(entry.get("index", 0))
                annotations[idx] = {
                    "confidence": entry.get("confidence", "medium"),
                    "conflict": bool(entry.get("conflict", False)),
                }
        except (json.JSONDecodeError, ValueError, AttributeError):
            log.warning("research.cite_json_unparseable", snippet=raw[:200])

    markers: list[tuple[int, int]] = []  # (marker_position, index)
    for match in re.finditer(r"\[(\d{1,3})\]", report):
        index = int(match.group(1))
        if 1 <= index <= source_count:
            markers.append((match.start(), index))

    citations: list[dict[str, Any]] = []
    seen_indexes: set[int] = set()
    for pos, index in markers:
        if index in seen_indexes:
            continue
        seen_indexes.add(index)
        claim = _sentence_around(report, pos)
        annotation = annotations.get(index, {})
        citations.append(
            {
                "index": index,
                "text": claim[:400],
                "source_ids": [index],
                "confidence": annotation.get("confidence", "medium"),
                "conflict": bool(annotation.get("conflict", False)),
            }
        )
    return citations


def _sentence_around(text: str, position: int, max_len: int = 400) -> str:
    """The sentence containing ``position``, trimmed to sentence bounds.

    ``.`` only counts as a terminator when followed by whitespace, so decimal
    numbers like ``2.0`` don't split sentences.
    """
    before = text[:position]
    start = 0
    for match in _SENT_BOUNDARY_RE.finditer(before):
        start = match.end()
    after = text[position:]
    after_match = _SENT_BOUNDARY_RE.search(after)
    end = position + (after_match.end() if after_match else len(after))
    return text[start:end].strip()[:max_len]


# --- Tool / task entry points ----------------------------------------------


async def run_research_inline(
    *,
    topic: str,
    depth: int = 4,
    model: str | None = None,
    conversation_id: int | None = None,
) -> tuple[str | None, int | None]:
    """Create + execute a research run inline (for the ``deep_research`` tool).

    Returns (report_markdown, research_run_id). Executes synchronously within
    the caller's task so the agent loop sees the full result as tool output.
    """
    with Session(engine) as session:
        run = start_research(
            session,
            topic=topic,
            depth=depth,
            model=model,
            conversation_id=conversation_id,
        )
        run_id = run.id
    report = await execute_research(run_id, sink=None)
    return report, run_id


async def run_research_for_task(
    session: Session,
    *,
    task_run_id: int,
    conversation_id: int,
    topic: str,
    depth: int = 4,
    model: str | None = None,
) -> tuple[str | None, dict | None, str | None]:
    """Execute deep research as a recurring task (Фаза 3b integration).

    Returns (output, usage, error) so the tasks service can finalize the
    TaskRun exactly like an agent-loop run.
    """
    run = start_research(
        session,
        topic=topic,
        depth=depth,
        model=model,
        conversation_id=conversation_id,
        parent_task_run_id=task_run_id,
    )
    report = await execute_research(run.id, sink=None)
    usage = run.usage if run else None
    error = run.error if run and run.status == RESEARCH_STATUS_FAILED else None
    if report and len(report) > MAX_REPORT_OUTPUT_CHARS:
        report = report[:MAX_REPORT_OUTPUT_CHARS] + "\n[... truncated]"
    return report, usage, error


__all__ = [
    "EventSink",
    "cancel_research_run",
    "execute_research",
    "get_research_run",
    "list_research_runs",
    "rerun_research",
    "run_research_for_task",
    "run_research_inline",
    "start_research",
]
