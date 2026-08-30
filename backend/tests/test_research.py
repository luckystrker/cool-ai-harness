"""Tests for the deep research workflow (Фаза 4 — Deep Research).

Covers the pipeline orchestration (decompose → parallel researcher subagents →
source collection → synthesis with citations → artifact), cancellation, rerun,
source/citation parsing units, and the API (CRUD, SSE stream, export).
"""

from __future__ import annotations

import asyncio
import json

import pytest
from fastapi.testclient import TestClient
from sqlmodel import Session

from app.core.db import engine
from app.models.research import (
    RESEARCH_STATUS_CANCELLED,
    RESEARCH_STATUS_COMPLETED,
    RESEARCH_STATUS_FAILED,
    RESEARCH_STATUS_QUEUED,
    ResearchRun,
)
from app.providers import ChatResult, LLMProvider, Usage
from app.research import (
    cancel_research_run,
    execute_research,
    get_research_run,
    list_research_runs,
    rerun_research,
    research_registry,
    start_research,
)
from app.research.orchestrator import (
    EventSink,
    _extract_citations,
    _extract_sources,
)
from tests.conftest import ScriptedProvider


@pytest.fixture(autouse=True)
def _seed_user():
    """Ensure tables exist and a default user is present for FK constraints."""
    from app.agent.service import get_or_create_default_user
    from app.core.db import init_db

    init_db()
    with Session(engine) as session:
        get_or_create_default_user(session)


@pytest.fixture
def artifacts_tmp(tmp_path, monkeypatch) -> None:
    """Redirect artifact storage to a temp dir (keep data/ clean)."""
    from app.core import config as config_module

    monkeypatch.setattr(config_module.get_settings(), "artifacts_dir", tmp_path / "artifacts")


def _patch_provider(monkeypatch, provider) -> None:
    """Inject a provider into every module that resolves the provider.

    ``app.agent.subagents`` binds ``get_provider_for_model`` at import time,
    so it must be patched on the subagent module directly (same as
    test_subagents.py).
    """
    monkeypatch.setattr("app.providers.get_provider_for_model", lambda model=None: provider)
    monkeypatch.setattr(
        "app.research.orchestrator.get_provider_for_model", lambda model=None: provider
    )
    monkeypatch.setattr(
        "app.agent.subagents.get_provider_for_model", lambda model=None: provider
    )


class ChatScriptedProvider(ScriptedProvider):
    """ScriptedProvider + non-streaming chat_completion for decompose/synthesize."""

    def __init__(self) -> None:
        super().__init__()
        self.completions: list[str] = []

    def set_completions(self, texts: list[str]) -> None:
        self.completions = list(texts)

    async def chat_completion(self, messages, *, model, tools=None, **kwargs):  # type: ignore[override]
        self.calls.append(list(messages))
        if not self.completions:
            raise RuntimeError("ChatScriptedProvider: no completions left")
        text = self.completions.pop(0)
        return ChatResult(
            content=text,
            usage=Usage(prompt_tokens=20, completion_tokens=10, total_tokens=30, cost_usd=0.001),
        )


class BlockingProvider(LLMProvider):
    """Provider that blocks forever on chat_completion (for cancellation tests)."""

    name = "blocking"

    async def chat_completion(self, messages, *, model, tools=None, **kwargs):
        await asyncio.Event().wait()  # pragma: no cover - blocks until cancelled

    async def chat_completion_stream(self, messages, *, model, tools=None, **kwargs):
        yield None  # pragma: no cover - unused


# --- fixtures for scripted pipeline ----------------------------------------


FINDINGS_1 = (
    "1. The Alpha framework version 2.0 shipped last quarter https://example.com/a [1]\n"
    "2. Beta framework is a fork https://example.com/b\n"
    "\n"
    "## Sources\n"
    "https://example.com/a | Alpha framework docs\n"
    "https://example.com/b | Beta framework repo"
)
FINDINGS_2 = (
    "1. The Alpha framework is MIT licensed https://example.com/a\n"
    "## Sources\n"
    "https://example.com/a | Alpha framework docs"
)
FINDINGS_3 = (
    "1. Community adoption grew 40% https://example.com/c\n"
    "## Sources\n"
    "https://example.com/c | Community survey"
)
REPORT = (
    "# Alpha framework\n"
    "## Executive Summary\n"
    "Alpha 2.0 shipped last quarter [1].\n"
    "## Key Findings\n"
    "1. Adoption grew 40% [3].\n"
    "## Detailed Analysis\n"
    "### Q1\n"
    "Version 2.0 shipped [1]; it is a fork of Beta [2].\n"
    "### Q2\n"
    "MIT licensed [1].\n"
    "## Limitations & Gaps\n"
    "Nothing to verify.\n"
    "## Sources\n"
    "[1] https://example.com/a\n"
    "[2] https://example.com/b\n"
    "[3] https://example.com/c\n"
    'CITE_JSON: {"citations": [{"index": 1, "confidence": "high", "conflict": false}, '
    '{"index": 2, "confidence": "medium", "conflict": true}, '
    '{"index": 3, "confidence": "low", "conflict": false}]}'
)


def _script_pipeline(provider: ChatScriptedProvider, *, depth: int = 3) -> None:
    provider.set_completions(
        [
            "1. What is the Alpha framework?\n2. What is Beta?\n3. How fast is adoption?",
            REPORT,
        ]
    )
    provider.set_script([FINDINGS_1, FINDINGS_2, FINDINGS_3][:depth])


# --- orchestration ---------------------------------------------------------


@pytest.mark.asyncio
async def test_full_pipeline_produces_cited_report(
    monkeypatch, artifacts_tmp
) -> None:
    provider = ChatScriptedProvider()
    _script_pipeline(provider)
    _patch_provider(monkeypatch, provider)

    with Session(engine) as session:
        run = start_research(session, topic="Alpha framework", depth=3, model="test-model")
        run_id = run.id

    queue: asyncio.Queue[dict] = asyncio.Queue()
    report = await execute_research(run_id, sink=EventSink.for_queue(queue))

    with Session(engine) as session:
        run = get_research_run(session, run_id)
        assert run is not None
        assert run.status == RESEARCH_STATUS_COMPLETED
        assert run.sub_questions == [
            "What is the Alpha framework?",
            "What is Beta?",
            "How fast is adoption?",
        ]
        assert len(run.sources) == 3  # a/b/c, deduped across subagents
        assert {s["url"] for s in run.sources} == {
            "https://example.com/a",
            "https://example.com/b",
            "https://example.com/c",
        }
        assert run.sources[0]["title"] == "Alpha framework docs"
        assert run.sources[0]["confidence"] == "high"
        assert run.report_artifact_id is not None
        assert run.usage["total_tokens"] == 60  # decompose + synthesize
        assert run.usage["cost_usd"] == pytest.approx(0.002)

    assert report is not None
    assert "Alpha 2.0 shipped last quarter [1]" in report
    assert "## Sources" in report

    with Session(engine) as session:
        run = session.get(ResearchRun, run_id)
        citations = run.citations or []
    assert len(citations) == 3
    by_index = {c["index"]: c for c in citations}
    assert by_index[1]["confidence"] == "high"
    assert by_index[2]["conflict"] is True
    assert by_index[2]["text"].startswith("Version 2.0 shipped")
    assert by_index[1]["source_ids"] == [1]

    # Event stream covered every stage.
    events = _drain(queue)
    types = [e["type"] for e in events]
    stage_names = [e["payload"]["stage"] for e in events if e["type"] == "stage"]
    assert "decompose" in stage_names and "gather" in stage_names and "synthesize" in stage_names
    assert "subquestion_started" in types
    assert types.count("subquestion_completed") == 3
    assert types.count("source_found") == 3
    assert "completed" in types

    # Artifact landed on disk.
    from app.artifacts import get_artifact, get_artifact_file

    with Session(engine) as session:
        art = get_artifact(session, run.report_artifact_id)
        assert art is not None and art.kind == "report"
        assert get_artifact_file(art) is not None


def _drain(queue: asyncio.Queue) -> list[dict]:
    events = []
    while not queue.empty():
        events.append(queue.get_nowait())
    return events


@pytest.mark.asyncio
async def test_decompose_failure_marks_run_failed(monkeypatch, artifacts_tmp) -> None:
    provider = ChatScriptedProvider()  # no completions → decompose raises
    _patch_provider(monkeypatch, provider)
    provider.set_script(["unused"])  # subagents never reached

    with Session(engine) as session:
        run = start_research(session, topic="Doomed topic", depth=3, model="test-model")
        run_id = run.id

    result = await execute_research(run_id, sink=None)
    assert result is None

    with Session(engine) as session:
        run = get_research_run(session, run_id)
        assert run is not None
        assert run.status == RESEARCH_STATUS_FAILED
        assert "completions" in (run.error or "")


@pytest.mark.asyncio
async def test_cancel_marks_run_cancelled(monkeypatch) -> None:
    provider = BlockingProvider()
    _patch_provider(monkeypatch, provider)

    with Session(engine) as session:
        run = start_research(session, topic="Blocking topic", depth=3, model="test-model")
        run_id = run.id

    task = asyncio.ensure_future(execute_research(run_id, sink=None))
    research_registry.register(run_id, task)

    # Give the pipeline time to reach the blocking decompose call.
    await asyncio.sleep(0.05)
    with Session(engine) as session:
        assert cancel_research_run(session, run_id) is True

    with pytest.raises(asyncio.CancelledError):
        await task

    with Session(engine) as session:
        run = get_research_run(session, run_id)
        assert run is not None
        assert run.status == RESEARCH_STATUS_CANCELLED
        assert run.finished_at is not None


def test_rerun_keeps_inputs_and_new_id() -> None:
    with Session(engine) as session:
        run = start_research(session, topic="Same topic", depth=4, model="m1")
        run_id = run.id
        rerun = rerun_research(session, run_id=run_id, model="m2")
        assert rerun.id != run_id
        assert rerun.topic == "Same topic"
        assert rerun.depth == 4
        assert rerun.model == "m2"
        assert rerun.input_hash == start_research(
            session, topic="Same topic", depth=4, model="m2"
        ).input_hash


def test_browser_activity_is_scoped_to_exact_research_run() -> None:
    from app.agent.service import append_run_events
    from app.agent.subagents import create_subagent_run
    from app.api.research import _browser_activity

    with Session(engine) as session:
        first = start_research(session, topic="First", depth=3, model="test-model")
        assert first.id is not None and first.conversation_id is not None
        second = start_research(
            session,
            topic="Second",
            depth=3,
            model="test-model",
            conversation_id=first.conversation_id,
        )
        assert second.id is not None
        first_child = create_subagent_run(
            session,
            prompt="first",
            parent_conversation_id=first.conversation_id,
            model_override="test-model",
            research_run_id=first.id,
        )
        second_child = create_subagent_run(
            session,
            prompt="second",
            parent_conversation_id=first.conversation_id,
            model_override="test-model",
            research_run_id=second.id,
        )
        assert first_child.run_id is not None and second_child.run_id is not None
        append_run_events(
            session,
            run_id=first_child.run_id,
            events=[
                (
                    "tool_call_start",
                    {"id": "first-call", "name": "browser_navigate", "arguments": {}},
                )
            ],
        )
        append_run_events(
            session,
            run_id=second_child.run_id,
            events=[
                (
                    "tool_call_start",
                    {"id": "second-call", "name": "browser_navigate", "arguments": {}},
                )
            ],
        )

        activity = _browser_activity(session, first.id)
        assert [item["id"] for item in activity] == ["first-call"]


def test_cancel_terminal_run_returns_false() -> None:
    with Session(engine) as session:
        run = start_research(session, topic="X", depth=3)
        run.status = RESEARCH_STATUS_COMPLETED
        session.add(run)
        session.commit()
        assert cancel_research_run(session, run.id) is False


def test_list_runs_newest_first() -> None:
    with Session(engine) as session:
        a = start_research(session, topic="A", depth=3)
        b = start_research(session, topic="B", depth=3)
        ids = [r.id for r in list_research_runs(session)]
    assert ids[0] == b.id and ids[1] == a.id


# --- parsing units ---------------------------------------------------------


def test_extract_sources_parses_urls_titles_snippets() -> None:
    sources = _extract_sources(FINDINGS_1, sub_question="Q")
    assert len(sources) == 2
    assert sources[0]["url"] == "https://example.com/a"
    assert sources[0]["title"] == "Alpha framework docs"
    assert sources[0]["confidence"] == "high"
    assert "Alpha framework version 2.0" in sources[0]["snippet"]
    assert sources[1]["url"] == "https://example.com/b"
    assert sources[1]["title"] == "Beta framework repo"


def test_extract_citations_parses_markers_and_annotations() -> None:
    citations = _extract_citations(REPORT, source_count=3)
    assert len(citations) == 3
    by_index = {c["index"]: c for c in citations}
    assert by_index[1]["confidence"] == "high"
    assert by_index[2]["confidence"] == "medium"
    assert by_index[2]["conflict"] is True
    assert by_index[3]["confidence"] == "low"
    assert by_index[1]["text"].startswith("Alpha 2.0 shipped last quarter")


def test_extract_citations_ignores_out_of_range_indexes() -> None:
    citations = _extract_citations("Claim [9] here.", source_count=2)
    assert citations == []


# --- API -------------------------------------------------------------------


def test_api_crud_and_export(monkeypatch, artifacts_tmp) -> None:
    from app.main import app

    provider = ChatScriptedProvider()
    _script_pipeline(provider)
    _patch_provider(monkeypatch, provider)

    with TestClient(app) as c:
        # Background start (no loop in the sync endpoint → stays queued).
        resp = c.post(
            "/api/research",
            json={"topic": "API topic", "depth": 3, "model": "test-model"},
        )
        assert resp.status_code == 201
        body = resp.json()
        assert body["topic"] == "API topic"
        assert body["status"] == RESEARCH_STATUS_QUEUED
        run_id = body["id"]

        assert c.post("/api/research", json={"topic": "  "}).status_code == 400
        assert c.post("/api/research", json={"topic": "X", "depth": 99}).status_code == 201

        listing = c.get("/api/research").json()
        assert any(r["id"] == run_id for r in listing)

        detail = c.get(f"/api/research/{run_id}").json()
        assert detail["sub_questions"] == []

        assert c.get("/api/research/999999").status_code == 404
        assert c.post("/api/research/999999/cancel").status_code == 404
        assert c.post("/api/research/999999/rerun", json={}).status_code == 404

    # Completed run (created directly) so export has a report to serve.
    with Session(engine) as session:
        completed = start_research(session, topic="Export topic", depth=3, model="test-model")
        completed.status = RESEARCH_STATUS_COMPLETED
        completed.report_markdown = "# Export topic\n\nBody [1].\n\n## Sources\n[1] https://x.com"
        session.add(completed)
        session.commit()
        session.refresh(completed)
        completed_id = completed.id

    with TestClient(app) as c:
        md = c.get(f"/api/research/{completed_id}/export?format=md")
        assert md.status_code == 200
        assert "## Sources" in md.text
        html_resp = c.get(f"/api/research/{completed_id}/export?format=html")
        assert "<h1>" in html_resp.text
        assert "<html" in html_resp.text
        assert c.get("/api/research/999999/export").status_code == 404


def test_api_stream_emits_progress_events(monkeypatch, artifacts_tmp) -> None:
    from app.main import app

    provider = ChatScriptedProvider()
    _script_pipeline(provider)
    _patch_provider(monkeypatch, provider)

    with TestClient(app) as c, c.stream(
        "POST",
        "/api/research/stream",
        json={"topic": "Stream topic", "depth": 3, "model": "test-model"},
        headers={"Accept": "text/event-stream"},
    ) as resp:
        assert resp.status_code == 200
        events = []
        for line in resp.iter_lines():
            if line.startswith("data:"):
                events.append(json.loads(line[len("data:") :].strip()))

    types = [e["type"] for e in events]
    assert "completed" in types
    assert "subquestion_started" in types
    assert types.count("subquestion_completed") == 3
    assert "source_found" in types

    # The run is durable and finished after the stream.
    with Session(engine) as session:
        runs = list_research_runs(session, limit=5)
        stream_run = next(r for r in runs if r.topic == "Stream topic")
        assert stream_run.status == RESEARCH_STATUS_COMPLETED
        assert stream_run.report_markdown and "## Sources" in stream_run.report_markdown
