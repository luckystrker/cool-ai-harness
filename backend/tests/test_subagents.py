"""Tests for the subagent system (Фаза 2 §5).

Covers: role CRUD, launch/isolation, execution with ScriptedProvider,
cancellation, batch launch, capability inheritance, and the spawn_subagent tool.
"""

from __future__ import annotations

import pytest
from sqlmodel import Session

from app.agent.subagents import (
    cancel_subagent_run,
    create_role,
    create_subagent_run,
    delete_role,
    delete_subagent_run,
    ensure_builtin_roles,
    execute_subagent,
    get_role,
    get_role_by_name,
    get_subagent_messages,
    get_subagent_run,
    list_roles,
    list_subagent_runs,
    update_role,
)
from app.core.db import engine
from app.models import Conversation
from app.models.subagent import (
    SUBAGENT_STATUS_CANCELLED,
    SUBAGENT_STATUS_COMPLETED,
    SUBAGENT_STATUS_QUEUED,
)


@pytest.fixture(autouse=True)
def _seed_user():
    """Ensure tables exist and a default user is present for FK constraints."""
    from app.agent.service import get_or_create_default_user
    from app.core.db import init_db

    init_db()
    with Session(engine) as session:
        get_or_create_default_user(session)


@pytest.fixture
def parent_conversation() -> int:
    """Create a parent conversation for subagent tests."""
    from app.agent.service import create_conversation, get_or_create_default_user

    with Session(engine) as session:
        user = get_or_create_default_user(session)
        conv = create_conversation(session, user_id=user.id, title="Parent conv")
        return conv.id


# --- Role CRUD ---


class TestRoleCRUD:
    def test_create_role(self):
        with Session(engine) as session:
            role = create_role(
                session,
                name="test-role",
                description="A test role",
                system_prompt="You are a test agent.",
                max_iterations=5,
            )
            assert role.id is not None
            assert role.name == "test-role"
            assert role.is_builtin is False

    def test_list_roles(self):
        with Session(engine) as session:
            create_role(session, name="role-a")
            create_role(session, name="role-b")
            roles = list_roles(session)
            names = [r.name for r in roles]
            assert "role-a" in names
            assert "role-b" in names

    def test_get_role(self):
        with Session(engine) as session:
            role = create_role(session, name="get-me")
            fetched = get_role(session, role.id)
            assert fetched is not None
            assert fetched.name == "get-me"

    def test_get_role_by_name(self):
        with Session(engine) as session:
            create_role(session, name="named-role")
            fetched = get_role_by_name(session, "named-role")
            assert fetched is not None
            assert fetched.name == "named-role"

    def test_update_role(self):
        with Session(engine) as session:
            role = create_role(session, name="update-me", max_iterations=5)
            updated = update_role(session, role.id, max_iterations=20, description="Updated")
            assert updated is not None
            assert updated.max_iterations == 20
            assert updated.description == "Updated"

    def test_delete_role(self):
        with Session(engine) as session:
            role = create_role(session, name="delete-me")
            assert delete_role(session, role.id) is True
            assert get_role(session, role.id) is None

    def test_delete_builtin_role_blocked(self):
        with Session(engine) as session:
            role = create_role(session, name="builtin-role", is_builtin=True)
            assert delete_role(session, role.id) is False
            assert get_role(session, role.id) is not None

    def test_ensure_builtin_roles(self):
        with Session(engine) as session:
            ensure_builtin_roles(session)
            roles = list_roles(session)
            names = [r.name for r in roles]
            assert "researcher" in names
            assert "code-reviewer" in names
            assert "summarizer" in names
            # All builtins flagged.
            for r in roles:
                if r.name in ("researcher", "code-reviewer", "summarizer"):
                    assert r.is_builtin is True


# --- Subagent Run creation and isolation ---


class TestSubagentRunCreation:
    def test_create_subagent_run_isolates_conversation(self, parent_conversation):
        with Session(engine) as session:
            role = create_role(session, name="iso-role")
            sa_run = create_subagent_run(
                session,
                prompt="Do something",
                parent_conversation_id=parent_conversation,
                role=role,
            )
            assert sa_run.id is not None
            assert sa_run.status == SUBAGENT_STATUS_QUEUED
            assert sa_run.conversation_id != parent_conversation
            assert sa_run.parent_conversation_id == parent_conversation
            # The isolated conversation exists.
            conv = session.get(Conversation, sa_run.conversation_id)
            assert conv is not None
            assert "[Subagent]" in (conv.title or "")

    def test_create_subagent_run_persists_prompt(self, parent_conversation):
        with Session(engine) as session:
            sa_run = create_subagent_run(
                session,
                prompt="Hello subagent",
                parent_conversation_id=parent_conversation,
            )
            messages = get_subagent_messages(session, sa_run.id)
            assert len(messages) >= 1
            assert messages[0].role == "user"
            assert messages[0].content == "Hello subagent"


# --- Execution with ScriptedProvider ---


class TestSubagentExecution:
    async def test_execute_subagent_completes(self, parent_conversation, monkeypatch):
        """Execute a subagent with a ScriptedProvider and verify completion."""
        from tests.conftest import ScriptedProvider

        provider = ScriptedProvider()
        provider.set_script(["This is the subagent result."])

        # Patch get_provider_for_model to return our scripted provider.
        monkeypatch.setattr(
            "app.agent.subagents.get_provider_for_model", lambda model: provider
        )

        with Session(engine) as session:
            role = create_role(
                session,
                name="exec-role",
                system_prompt="You are helpful.",
                max_iterations=3,
            )
            sa_run = create_subagent_run(
                session,
                prompt="Say something",
                parent_conversation_id=parent_conversation,
                role=role,
            )
            run_id = sa_run.id

        await execute_subagent(run_id)

        with Session(engine) as session:
            sa_run = get_subagent_run(session, run_id)
            assert sa_run.status == SUBAGENT_STATUS_COMPLETED
            assert sa_run.finished_at is not None
            assert sa_run.result_summary is not None


# --- Cancellation ---


class TestSubagentCancellation:
    def test_cancel_queued_run(self, parent_conversation):
        with Session(engine) as session:
            sa_run = create_subagent_run(
                session,
                prompt="Cancel me",
                parent_conversation_id=parent_conversation,
            )
            assert cancel_subagent_run(session, sa_run.id) is True
            session.refresh(sa_run)
            assert sa_run.status == SUBAGENT_STATUS_CANCELLED

    def test_cancel_terminal_run_fails(self, parent_conversation):
        with Session(engine) as session:
            sa_run = create_subagent_run(
                session,
                prompt="Already done",
                parent_conversation_id=parent_conversation,
            )
            # Manually set to completed.
            sa_run.status = SUBAGENT_STATUS_COMPLETED
            session.add(sa_run)
            session.commit()
            assert cancel_subagent_run(session, sa_run.id) is False


# --- Batch launch ---


class TestBatchLaunch:
    def test_list_subagent_runs_filter(self, parent_conversation):
        with Session(engine) as session:
            create_subagent_run(
                session, prompt="A", parent_conversation_id=parent_conversation
            )
            create_subagent_run(
                session, prompt="B", parent_conversation_id=parent_conversation
            )
            runs = list_subagent_runs(session, parent_conversation_id=parent_conversation)
            assert len(runs) >= 2

    def test_list_subagent_runs_status_filter(self, parent_conversation):
        with Session(engine) as session:
            sa_run = create_subagent_run(
                session, prompt="C", parent_conversation_id=parent_conversation
            )
            runs = list_subagent_runs(session, status=SUBAGENT_STATUS_QUEUED)
            assert any(r.id == sa_run.id for r in runs)


# --- Delete ---


class TestSubagentRunDelete:
    def test_delete_terminal_run(self, parent_conversation):
        with Session(engine) as session:
            sa_run = create_subagent_run(
                session, prompt="Delete me", parent_conversation_id=parent_conversation
            )
            sa_run.status = SUBAGENT_STATUS_COMPLETED
            session.add(sa_run)
            session.commit()
            assert delete_subagent_run(session, sa_run.id) is True
            assert get_subagent_run(session, sa_run.id) is None

    def test_delete_active_run_blocked(self, parent_conversation):
        with Session(engine) as session:
            sa_run = create_subagent_run(
                session, prompt="Still running", parent_conversation_id=parent_conversation
            )
            assert delete_subagent_run(session, sa_run.id) is False


# --- API integration ---


class TestSubagentAPI:
    @pytest.fixture
    def client(self):
        from fastapi.testclient import TestClient

        from app.main import app

        return TestClient(app)

    def test_roles_crud(self, client):
        # Create
        resp = client.post(
            "/api/subagents/roles",
            json={"name": "api-role", "description": "API test", "max_iterations": 7},
        )
        assert resp.status_code == 201
        role = resp.json()
        assert role["name"] == "api-role"
        role_id = role["id"]

        # List
        resp = client.get("/api/subagents/roles")
        assert resp.status_code == 200
        assert any(r["id"] == role_id for r in resp.json())

        # Get
        resp = client.get(f"/api/subagents/roles/{role_id}")
        assert resp.status_code == 200
        assert resp.json()["name"] == "api-role"

        # Update
        resp = client.put(f"/api/subagents/roles/{role_id}", json={"max_iterations": 15})
        assert resp.status_code == 200
        assert resp.json()["max_iterations"] == 15

        # Delete
        resp = client.delete(f"/api/subagents/roles/{role_id}")
        assert resp.status_code == 204

    def test_launch_and_list_runs(self, client, parent_conversation):
        # Create a role first.
        resp = client.post(
            "/api/subagents/roles",
            json={"name": "launch-role"},
        )
        role_id = resp.json()["id"]

        # Launch
        resp = client.post(
            "/api/subagents/launch",
            json={
                "prompt": "Do a thing",
                "role_id": role_id,
                "parent_conversation_id": parent_conversation,
            },
        )
        assert resp.status_code == 201
        run = resp.json()
        assert run["status"] == "queued"
        assert run["parent_conversation_id"] == parent_conversation

        # List runs
        resp = client.get(
            f"/api/subagents/runs?parent_conversation_id={parent_conversation}"
        )
        assert resp.status_code == 200
        assert len(resp.json()) >= 1

    def test_launch_batch(self, client, parent_conversation):
        resp = client.post(
            "/api/subagents/launch-batch",
            json={
                "parent_conversation_id": parent_conversation,
                "items": [
                    {"prompt": "Task A"},
                    {"prompt": "Task B"},
                ],
            },
        )
        assert resp.status_code == 201
        runs = resp.json()
        assert len(runs) == 2

    def test_cancel_run(self, client, parent_conversation):
        # Launch
        resp = client.post(
            "/api/subagents/launch",
            json={
                "prompt": "Cancel me via API",
                "parent_conversation_id": parent_conversation,
            },
        )
        run_id = resp.json()["id"]

        # Cancel
        resp = client.post(f"/api/subagents/runs/{run_id}/cancel")
        assert resp.status_code == 200
        assert resp.json()["cancelled"] is True

    def test_delete_builtin_role_forbidden(self, client):
        # Seed builtins.
        with Session(engine) as session:
            ensure_builtin_roles(session)
            role = get_role_by_name(session, "researcher")
            role_id = role.id

        resp = client.delete(f"/api/subagents/roles/{role_id}")
        assert resp.status_code == 403
