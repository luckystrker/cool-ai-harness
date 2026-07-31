"""Tests for the conversations API: CRUD endpoints."""

from __future__ import annotations

from fastapi.testclient import TestClient


def _client() -> TestClient:
    from app.main import app

    return TestClient(app)


def test_create_list_get_delete_conversation() -> None:
    with _client() as c:
        # Create
        resp = c.post("/api/conversations", json={"title": "My chat"})
        assert resp.status_code == 200, resp.text
        conv = resp.json()
        conv_id = conv["id"]
        assert conv["title"] == "My chat"

        # List
        resp = c.get("/api/conversations")
        assert resp.status_code == 200
        assert any(c["id"] == conv_id for c in resp.json())

        # Get detail (with messages — empty so far)
        resp = c.get(f"/api/conversations/{conv_id}")
        assert resp.status_code == 200
        detail = resp.json()
        assert detail["id"] == conv_id
        assert detail["messages"] == []

        # Delete
        resp = c.delete(f"/api/conversations/{conv_id}")
        assert resp.status_code == 200

        # Now 404
        resp = c.get(f"/api/conversations/{conv_id}")
        assert resp.status_code == 404


def test_get_missing_conversation_404() -> None:
    with _client() as c:
        resp = c.get("/api/conversations/999999")
        assert resp.status_code == 404


def test_patch_conversation_updates_model_and_title() -> None:
    with _client() as c:
        resp = c.post("/api/conversations", json={})
        assert resp.status_code == 200, resp.text
        conv_id = resp.json()["id"]

        # Patch only the model.
        resp = c.patch(f"/api/conversations/{conv_id}", json={"model": "gpt-4o"})
        assert resp.status_code == 200, resp.text
        assert resp.json()["model"] == "gpt-4o"

        # Title should be unchanged by the model-only patch.
        resp = c.get(f"/api/conversations/{conv_id}")
        assert resp.status_code == 200
        detail = resp.json()
        assert detail["model"] == "gpt-4o"
        assert detail["title"] is None

        # Now patch the title too.
        resp = c.patch(
            f"/api/conversations/{conv_id}", json={"title": "Renamed"}
        )
        assert resp.status_code == 200
        patched = resp.json()
        assert patched["title"] == "Renamed"
        # Model must persist from the earlier patch.
        assert patched["model"] == "gpt-4o"


def test_patch_missing_conversation_404() -> None:
    with _client() as c:
        resp = c.patch("/api/conversations/999999", json={"model": "x"})
        assert resp.status_code == 404


# --- working directory + permissions --------------------------------------


def test_create_with_permissions_and_workdir() -> None:
    with _client() as c:
        resp = c.post(
            "/api/conversations",
            json={
                "title": "gated",
                "working_directory": "/tmp/agent-x",
                "permissions": {"*": "ask", "read_file": "allow"},
            },
        )
        assert resp.status_code == 200, resp.text
        body = resp.json()
        assert body["working_directory"] == "/tmp/agent-x"
        assert body["permissions"] == {"*": "ask", "read_file": "allow"}


def test_create_rejects_invalid_permissions() -> None:
    with _client() as c:
        resp = c.post(
            "/api/conversations",
            json={"permissions": {"read_file": "maybe"}},
        )
        assert resp.status_code == 400
        assert "allow|ask|deny" in resp.text


def test_patch_permissions_and_workdir() -> None:
    with _client() as c:
        cid = c.post("/api/conversations", json={}).json()["id"]
        resp = c.patch(
            f"/api/conversations/{cid}",
            json={
                "working_directory": "/tmp/agent-y",
                "permissions": {"python_execute": "deny"},
            },
        )
        assert resp.status_code == 200, resp.text
        body = resp.json()
        assert body["working_directory"] == "/tmp/agent-y"
        assert body["permissions"] == {"python_execute": "deny"}


def test_patch_rejects_invalid_permissions() -> None:
    with _client() as c:
        cid = c.post("/api/conversations", json={}).json()["id"]
        resp = c.patch(
            f"/api/conversations/{cid}",
            json={"permissions": {"x": "nope"}},
        )
        assert resp.status_code == 400


def test_patch_can_clear_permissions() -> None:
    with _client() as c:
        cid = c.post(
            "/api/conversations", json={"permissions": {"*": "ask"}}
        ).json()["id"]
        resp = c.patch(f"/api/conversations/{cid}", json={"permissions": {}})
        assert resp.status_code == 200
        assert resp.json()["permissions"] is None


# --- approval endpoint ----------------------------------------------------


def test_approval_resolves_pending_request() -> None:
    """Register a pending approval, resolve it via the endpoint."""
    import asyncio

    from app.agent.approvals import approval_registry

    with _client() as c:
        cid = c.post("/api/conversations", json={}).json()["id"]

    # Simulate the executor having registered a pending approval.
    async def _setup() -> None:
        approval_registry.register("call_xyz", conversation_id=cid)

    asyncio.run(_setup())

    with _client() as c:
        resp = c.post(
            f"/api/conversations/{cid}/tool_calls/call_xyz/approval",
            json={"approved": True},
        )
        assert resp.status_code == 200, resp.text
        body = resp.json()
        assert body["resolved"] is True
        assert body["approved"] is True

        # A second resolve finds nothing pending → 404.
        resp = c.post(
            f"/api/conversations/{cid}/tool_calls/call_xyz/approval",
            json={"approved": True},
        )
        assert resp.status_code == 404


def test_approval_unknown_conversation_404() -> None:
    with _client() as c:
        resp = c.post(
            "/api/conversations/999999/tool_calls/whatever/approval",
            json={"approved": True},
        )
        assert resp.status_code == 404


def test_approval_deny() -> None:
    """Denying resolves the Future to False."""
    import asyncio

    from app.agent.approvals import approval_registry

    with _client() as c:
        cid = c.post("/api/conversations", json={}).json()["id"]

    future_holder: dict = {}

    async def _setup() -> None:
        future_holder["f"] = approval_registry.register("call_deny", conversation_id=cid)

    asyncio.run(_setup())

    with _client() as c:
        resp = c.post(
            f"/api/conversations/{cid}/tool_calls/call_deny/approval",
            json={"approved": False},
        )
        assert resp.status_code == 200
        assert resp.json()["approved"] is False

    # The Future the executor would be awaiting resolves to False.
    assert future_holder["f"].result() is False


# --- compact endpoint ---


class _SummaryProvider:
    """Minimal non-streaming provider double for the compact endpoint."""

    def __init__(self, summary: str = "A concise summary of the older messages.") -> None:
        self.summary = summary
        self.calls: list[list] = []

    async def chat_completion(self, messages, *, model, **kwargs):
        from app.providers import ChatResult

        self.calls.append(list(messages))
        return ChatResult(content=self.summary)


def test_compact_too_few_messages_skipped() -> None:
    with _client() as c:
        cid = c.post("/api/conversations", json={"model": "test-model"}).json()["id"]

    from sqlmodel import Session

    from app.agent.service import append_message
    from app.core.db import engine

    with Session(engine) as session:
        for i in range(5):
            role = "user" if i % 2 == 0 else "assistant"
            append_message(session, conversation_id=cid, role=role, content=f"msg {i}")

    with _client() as c:
        resp = c.post(f"/api/conversations/{cid}/compact")
        assert resp.status_code == 200, resp.text
        body = resp.json()
        assert body["status"] == "skipped"
        assert "Too few messages" in body["reason"]


def test_compact_summarizes_and_trims_history(monkeypatch) -> None:
    """Compact stores a WM summary and load_history skips compacted rows."""
    fake = _SummaryProvider()
    monkeypatch.setattr("app.api.conversations.get_provider_for_model", lambda m: fake)

    with _client() as c:
        cid = c.post("/api/conversations", json={"model": "test-model"}).json()["id"]

    from sqlmodel import Session

    from app.agent.service import append_message, load_history
    from app.core.db import engine

    with Session(engine) as session:
        for i in range(32):
            role = "user" if i % 2 == 0 else "assistant"
            append_message(session, conversation_id=cid, role=role, content=f"msg {i}")

    with _client() as c:
        resp = c.post(f"/api/conversations/{cid}/compact")
        assert resp.status_code == 200, resp.text
        body = resp.json()
        assert body["status"] == "compacted"
        assert body["messages_compacted"] == 22  # 32 - 10 kept
        assert body["messages_kept"] == 10
        assert body["summary_length"] == len(fake.summary)

    # The LLM was called once with a summarization prompt.
    assert len(fake.calls) == 1

    # Conversation detail now exposes the compaction state for the UI.
    with _client() as c:
        detail = c.get(f"/api/conversations/{cid}").json()
        assert detail["compact_summary"] == fake.summary
        assert detail["compact_up_to_message_id"] is not None
        # All messages are still returned (hidden in the UI, never deleted).
        assert len(detail["messages"]) == 32

    # History now contains only the 10 recent messages.
    with Session(engine) as session:
        history = load_history(session, cid)
        assert len(history) == 10
        assert history[0].content == "msg 22"
        assert history[-1].content == "msg 31"

        # The summary is stored in working memory with the correct cutoff.
        from app.memory.service import get_working_memory

        wm = get_working_memory(session, cid)
        assert wm is not None
        assert wm.summary == fake.summary
        assert wm.summary_up_to_message_id is not None


def test_compact_again_without_new_messages_skipped(monkeypatch) -> None:
    """A second compact right after the first has nothing new to summarize."""
    fake = _SummaryProvider()
    monkeypatch.setattr("app.api.conversations.get_provider_for_model", lambda m: fake)

    with _client() as c:
        cid = c.post("/api/conversations", json={"model": "test-model"}).json()["id"]

    from sqlmodel import Session

    from app.agent.service import append_message
    from app.core.db import engine

    with Session(engine) as session:
        for i in range(32):
            role = "user" if i % 2 == 0 else "assistant"
            append_message(session, conversation_id=cid, role=role, content=f"msg {i}")

    with _client() as c:
        first = c.post(f"/api/conversations/{cid}/compact").json()
        assert first["status"] == "compacted"

        second = c.post(f"/api/conversations/{cid}/compact").json()
        assert second["status"] == "skipped"
        assert "No new messages" in second["reason"]

    # The LLM was only called for the first compaction.
    assert len(fake.calls) == 1
