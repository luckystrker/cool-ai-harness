"""Cross-cutting tests for the completed Phase 4 capabilities."""

from __future__ import annotations

import base64
from pathlib import Path

import pytest
from fastapi.testclient import TestClient
from sqlmodel import Session, select

from app.agent.service import (
    append_message,
    create_conversation,
    get_or_create_default_user,
    load_history,
)
from app.artifacts import store_artifact
from app.core.db import engine
from app.models.macro_tool import MacroTool
from app.providers import Message
from app.providers.anthropic import AnthropicProvider
from app.providers.openai import OpenAIProvider
from app.tools.context import RunContext, reset_run_context, set_run_context

_PNG = base64.b64decode(
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII="
)


@pytest.fixture(autouse=True)
def _artifact_dir(tmp_path: Path, monkeypatch) -> Path:
    from app.core.config import get_settings

    path = tmp_path / "artifacts"
    path.mkdir()
    monkeypatch.setattr(get_settings(), "artifacts_dir", path)
    return path


def test_provider_adapters_translate_canonical_image_parts() -> None:
    message = Message(
        role="user",
        content=[
            {"type": "text", "text": "What is shown?"},
            {"type": "image", "media_type": "image/png", "data": "YWJj"},
        ],
    )
    openai = OpenAIProvider._message_to_payload(message)
    assert openai["content"][1]["type"] == "image_url"
    assert openai["content"][1]["image_url"]["url"] == "data:image/png;base64,YWJj"

    role, blocks = AnthropicProvider._message_to_blocks(message)
    assert role == "user"
    assert blocks[1] == {
        "type": "image",
        "source": {"type": "base64", "media_type": "image/png", "data": "YWJj"},
    }


def test_message_attachment_is_replayable_multimodal_history() -> None:
    with Session(engine) as session:
        user = get_or_create_default_user(session)
        assert user.id is not None
        conv = create_conversation(session, user_id=user.id, title="vision history")
        assert conv.id is not None
        artifact = store_artifact(
            session,
            conversation_id=conv.id,
            filename="pixel.png",
            content=_PNG,
            media_type="image/png",
        )
        assert artifact.id is not None
        append_message(
            session,
            conversation_id=conv.id,
            role="user",
            content="Analyze this",
            artifact_ids=[artifact.id],
        )
        history = load_history(session, conv.id)

    assert isinstance(history[-1].content, list)
    assert history[-1].content[0] == {"type": "text", "text": "Analyze this"}
    assert history[-1].content[1]["type"] == "image"
    assert history[-1].content[1]["data"] == base64.b64encode(_PNG).decode("ascii")


def test_attachment_ownership_is_enforced() -> None:
    from app.multimodal import validate_artifact_ids

    with Session(engine) as session:
        user = get_or_create_default_user(session)
        assert user.id is not None
        first = create_conversation(session, user_id=user.id, title="owner")
        second = create_conversation(session, user_id=user.id, title="other")
        assert first.id is not None and second.id is not None
        artifact = store_artifact(
            session,
            conversation_id=first.id,
            filename="pixel.png",
            content=_PNG,
            media_type="image/png",
        )
        assert artifact.id is not None
        with pytest.raises(ValueError, match="not found in this conversation"):
            validate_artifact_ids(
                session, conversation_id=second.id, artifact_ids=[artifact.id]
            )


def test_attachment_count_and_phase4_tool_capabilities_are_enforced() -> None:
    from app.multimodal import validate_artifact_ids
    from app.security.capabilities import Capability
    from app.tools import get_tool

    with Session(engine) as session, pytest.raises(ValueError, match="At most 10"):
        validate_artifact_ids(session, conversation_id=1, artifact_ids=list(range(11)))

    click = get_tool("browser_click")
    fill = get_tool("browser_fill")
    ocr = get_tool("ocr_extract")
    assert click is not None and click.dangerous
    assert fill is not None and fill.dangerous
    assert ocr is not None and ocr.dangerous
    assert Capability.SEND_EXTERNAL in (click.capabilities or ())
    assert Capability.SEND_EXTERNAL in (fill.capabilities or ())
    assert Capability.WRITE in (ocr.capabilities or ())


def test_constructor_macro_crud_clone_and_playground() -> None:
    from app.main import app

    with Session(engine) as session:
        old = session.exec(select(MacroTool).where(MacroTool.name == "macro_phase4_test")).first()
        if old is not None:
            session.delete(old)
            session.commit()

    with TestClient(app) as client:
        catalog = client.get("/api/agent-constructor/tools")
        assert catalog.status_code == 200
        assert any(tool["name"] == "browser_navigate" for tool in catalog.json())

        created = client.post(
            "/api/agent-constructor/macros",
            json={
                "name": "macro_phase4_test",
                "description": "Read a requested file",
                "input_schema": {
                    "type": "object",
                    "properties": {"path": {"type": "string"}},
                },
                "steps": [
                    {
                        "id": "step_1",
                        "tool_name": "read_file",
                        "arguments": {"path": "${input.path}"},
                    }
                ],
            },
        )
        assert created.status_code == 201, created.text
        macro_id = created.json()["id"]
        assert any(
            tool["name"] == "macro_phase4_test"
            for tool in client.get("/api/agent-constructor/tools").json()
        )

        profiles = client.get("/api/profiles").json()
        source = profiles[0]
        cloned = client.post(f"/api/profiles/{source['id']}/clone")
        assert cloned.status_code == 201
        assert cloned.json()["is_builtin"] is False
        playground = client.post(f"/api/profiles/{source['id']}/playground", json={})
        assert playground.status_code == 201
        assert playground.json()["conversation_id"] > 0

        assert client.delete(f"/api/agent-constructor/macros/{macro_id}").status_code == 200


async def test_browser_screenshot_becomes_artifact(monkeypatch) -> None:
    from app.tools.browser_tools import browser_screenshot, browser_sessions

    class Page:
        url = "https://example.com/chart"

        async def screenshot(self, **kwargs):
            return _PNG

    class FakeSession:
        page = Page()

    async def fake_get():
        return FakeSession()

    monkeypatch.setattr(browser_sessions, "get", fake_get)
    with Session(engine) as session:
        user = get_or_create_default_user(session)
        assert user.id is not None
        conv = create_conversation(session, user_id=user.id, title="browser screenshot")
        assert conv.id is not None

    token = set_run_context(RunContext(workdir=Path.cwd(), conversation_id=conv.id))
    try:
        result = await browser_screenshot()
    finally:
        reset_run_context(token)
    assert not result.is_error
    assert result.metadata["artifact_id"] > 0
    assert result.metadata["screenshot_url"].endswith("/download")


async def test_browser_blocks_private_navigation_before_launch() -> None:
    from app.tools.browser_tools import browser_navigate

    result = await browser_navigate(url="http://127.0.0.1/admin")
    assert result.is_error
    assert "SSRF" in result.output


async def test_browser_guard_blocks_cross_origin_and_oversized_resources(monkeypatch) -> None:
    from app.core.config import get_settings
    from app.tools.browser_tools import _BrowserSession, _route_with_ssrf_guard

    class Response:
        def __init__(self) -> None:
            self.headers = {"content-length": "4"}
            self.status = 200
            self.content = self

        async def __aenter__(self):
            return self

        async def __aexit__(self, *args):
            return None

        async def iter_chunked(self, size):
            yield b"data"

    class Http:
        def request(self, *args, **kwargs):
            return Response()

    class Route:
        def __init__(self) -> None:
            self.aborted: str | None = None
            self.fulfilled = False

        async def abort(self, reason):
            self.aborted = reason

        async def continue_(self):
            self.fulfilled = True

        async def fulfill(self, **kwargs):
            self.fulfilled = True

    class Request:
        def __init__(self, url: str) -> None:
            self.url = url
            self.headers = {}
            self.method = "GET"
            self.post_data_buffer = None

    session = _BrowserSession(
        None, None, None, None, 0.0, "example.com", "93.184.216.34", http=Http()
    )
    cross_origin = Route()
    await _route_with_ssrf_guard(
        cross_origin, Request("https://cdn.example.net/x.js"), session
    )
    assert cross_origin.aborted == "blockedbyclient"

    monkeypatch.setattr(get_settings(), "browser_max_resource_bytes", 3)
    oversized = Route()
    await _route_with_ssrf_guard(
        oversized, Request("https://example.com/x.js"), session
    )
    assert oversized.aborted == "blockedbyclient"


async def test_macro_schema_and_durable_nested_steps(tmp_path: Path) -> None:
    from app.agent.constructor import register_macro
    from app.agent.executor import AgentConfig, AgentExecutor
    from app.agent.permissions import PermissionsConfig
    from app.agent.service import create_run
    from app.models import ApprovalAudit, RunEvent, ToolCall
    from app.tools import get_registry, get_tool

    macro_name = "macro_durable_phase4"
    macro = MacroTool(
        id=987_654,
        name=macro_name,
        input_schema={
            "type": "object",
            "properties": {"path": {"type": "string"}},
            "required": ["path"],
            "additionalProperties": False,
        },
        steps=[
            {
                "id": "read_step",
                "tool_name": "read_file",
                "arguments": {"path": "${input.path}"},
            }
        ],
    )
    register_macro(macro)
    try:
        tool = get_tool(macro_name)
        assert tool is not None
        assert tool.composed_tools == ("read_file",)
        executor = AgentExecutor(
            provider=OpenAIProvider(base_url="https://example.invalid", api_key="test"),
            config=AgentConfig(
                model="test-model",
                permissions=PermissionsConfig({"read_file": "deny"}),
            ),
        )
        assert executor._resolve_decision(macro_name, dangerous=False) == "deny"
        invalid = await tool.run({})
        assert invalid.is_error
        assert "required" in invalid.output

        (tmp_path / "note.txt").write_text("durable", encoding="utf-8")
        with Session(engine) as session:
            user = get_or_create_default_user(session)
            assert user.id is not None
            conv = create_conversation(session, user_id=user.id, title="macro durable")
            assert conv.id is not None
            run = create_run(session, conversation_id=conv.id, model="test-model")
            assert run.id is not None
            conv_id, run_id = conv.id, run.id

        token = set_run_context(
            RunContext(workdir=tmp_path, conversation_id=conv_id, run_id=run_id)
        )
        try:
            result = await tool.run({"path": "note.txt"})
        finally:
            reset_run_context(token)
        assert not result.is_error
        assert result.output == "durable"

        with Session(engine) as session:
            events = session.exec(
                select(RunEvent).where(RunEvent.run_id == run_id)
            ).all()
            assert [event.kind for event in events] == [
                "tool_call_start",
                "tool_result",
            ]
            assert session.exec(
                select(ToolCall).where(
                    ToolCall.conversation_id == conv_id, ToolCall.name == "read_file"
                )
            ).first()
            audit = session.exec(
                select(ApprovalAudit).where(ApprovalAudit.run_id == run_id)
            ).first()
            assert audit is not None
            assert audit.decision_source == "macro_preflight"
    finally:
        get_registry().pop(macro_name, None)


def test_research_pdf_and_docx_exports_are_real_documents() -> None:
    from app.research.export import report_to_docx, report_to_pdf

    markdown = "# Findings\n\n- Evidence [source](https://example.com)\n\n## Conclusion\nDone."
    pdf = report_to_pdf(markdown, title="Phase 4 research")
    docx = report_to_docx(markdown, title="Phase 4 research")
    assert pdf.startswith(b"%PDF")
    assert docx.startswith(b"PK")
