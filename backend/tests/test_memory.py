"""Tests for the memory subsystem (Фаза 3a)."""

from __future__ import annotations

from datetime import UTC, datetime, timedelta

import pytest
from sqlmodel import Session, SQLModel, create_engine

from app.memory.models import (
    MEMORY_STATUS_ACTIVE,
    MEMORY_STATUS_ARCHIVED,
    MEMORY_TYPE_PREFERENCE,
    MEMORY_TYPE_SEMANTIC,
    SCOPE_AGENT,
    SCOPE_GLOBAL,
)


@pytest.fixture
def memory_session():
    """Create an in-memory SQLite session with memory tables."""
    engine = create_engine("sqlite://", echo=False)
    SQLModel.metadata.create_all(engine)
    with Session(engine) as session:
        yield session


@pytest.fixture
def user_id(memory_session: Session) -> int:
    """Create a test user and return its ID."""
    from app.models.user import User

    user = User(username="test", display_name="Test User")
    memory_session.add(user)
    memory_session.commit()
    memory_session.refresh(user)
    return user.id


# --- MemoryItem CRUD ---


class TestMemoryService:
    def test_remember_creates_memory(self, memory_session: Session, user_id: int):
        from app.memory.service import remember

        memory = remember(
            memory_session,
            user_id=user_id,
            content="User prefers Python over JavaScript",
            memory_type=MEMORY_TYPE_PREFERENCE,
            importance=0.8,
            source="user_explicit",
        )
        assert memory.id is not None
        assert memory.content == "User prefers Python over JavaScript"
        assert memory.memory_type == MEMORY_TYPE_PREFERENCE
        assert memory.importance == 0.8
        assert memory.status == MEMORY_STATUS_ACTIVE

    def test_remember_deduplicates_exact_match(self, memory_session: Session, user_id: int):
        from app.memory.service import remember

        m1 = remember(
            memory_session,
            user_id=user_id,
            content="Project uses FastAPI",
            memory_type=MEMORY_TYPE_SEMANTIC,
        )
        m2 = remember(
            memory_session,
            user_id=user_id,
            content="Project uses FastAPI",
            memory_type=MEMORY_TYPE_SEMANTIC,
        )
        # Should return the same memory (updated, not duplicated).
        assert m1.id == m2.id

    def test_remember_caps_agent_importance(self, memory_session: Session, user_id: int):
        from app.memory.service import remember

        memory = remember(
            memory_session,
            user_id=user_id,
            content="Test memory",
            importance=0.99,
            source="agent",
        )
        # Agent importance is capped at 0.9.
        assert memory.importance == 0.9

    def test_remember_user_explicit_allows_high_importance(
        self, memory_session: Session, user_id: int
    ):
        from app.memory.service import remember

        memory = remember(
            memory_session,
            user_id=user_id,
            content="Critical user preference",
            importance=0.99,
            source="user_explicit",
        )
        assert memory.importance == 0.99

    def test_get_memory(self, memory_session: Session, user_id: int):
        from app.memory.service import get_memory, remember

        created = remember(memory_session, user_id=user_id, content="Test")
        fetched = get_memory(memory_session, created.id)
        assert fetched is not None
        assert fetched.id == created.id

    def test_update_memory(self, memory_session: Session, user_id: int):
        from app.memory.service import remember, update_memory

        memory = remember(memory_session, user_id=user_id, content="Original")
        updated = update_memory(memory_session, memory.id, content="Updated", importance=0.9)
        assert updated is not None
        assert updated.content == "Updated"
        assert updated.importance == 0.9

    def test_forget_archives(self, memory_session: Session, user_id: int):
        from app.memory.service import forget, get_memory, remember

        memory = remember(memory_session, user_id=user_id, content="To forget")
        assert forget(memory_session, memory.id) is True
        archived = get_memory(memory_session, memory.id)
        assert archived is not None
        assert archived.status == MEMORY_STATUS_ARCHIVED

    def test_forget_hard_deletes(self, memory_session: Session, user_id: int):
        from app.memory.service import forget, get_memory, remember

        memory = remember(memory_session, user_id=user_id, content="To delete")
        memory_id = memory.id
        assert forget(memory_session, memory_id, hard=True) is True
        assert get_memory(memory_session, memory_id) is None

    def test_list_memories(self, memory_session: Session, user_id: int):
        from app.memory.service import list_memories, remember

        remember(
            memory_session, user_id=user_id, content="Memory 1", importance=0.5, source="user_explicit"
        )
        remember(
            memory_session, user_id=user_id, content="Memory 2", importance=0.9, source="user_explicit"
        )
        remember(
            memory_session, user_id=user_id, content="Memory 3", importance=0.3, source="user_explicit"
        )

        memories = list_memories(memory_session, user_id=user_id)
        assert len(memories) == 3
        # Should be ordered by importance desc.
        assert memories[0].importance >= memories[1].importance

    def test_list_memories_filter_by_type(self, memory_session: Session, user_id: int):
        from app.memory.service import list_memories, remember

        remember(
            memory_session,
            user_id=user_id,
            content="Fact",
            memory_type=MEMORY_TYPE_SEMANTIC,
            source="user_explicit",
        )
        remember(
            memory_session,
            user_id=user_id,
            content="Pref",
            memory_type=MEMORY_TYPE_PREFERENCE,
            source="user_explicit",
        )

        semantics = list_memories(
            memory_session, user_id=user_id, memory_type=MEMORY_TYPE_SEMANTIC
        )
        assert len(semantics) == 1
        assert semantics[0].memory_type == MEMORY_TYPE_SEMANTIC

    def test_get_preferences(self, memory_session: Session, user_id: int):
        from app.memory.service import get_preferences, remember

        remember(
            memory_session,
            user_id=user_id,
            content="Answer in Russian",
            memory_type=MEMORY_TYPE_PREFERENCE,
            source="user_explicit",
        )
        remember(
            memory_session,
            user_id=user_id,
            content="Project uses pytest",
            memory_type=MEMORY_TYPE_SEMANTIC,
            source="user_explicit",
        )

        prefs = get_preferences(memory_session, user_id=user_id)
        assert len(prefs) == 1
        assert prefs[0].content == "Answer in Russian"


# --- Scope visibility ---


class TestScopeVisibility:
    def test_global_visible_to_all(self, memory_session: Session, user_id: int):
        from app.memory.service import recall, remember

        remember(
            memory_session,
            user_id=user_id,
            content="Global fact",
            scope=SCOPE_GLOBAL,
            source="user_explicit",
        )
        # Should be visible without agent_id.
        results = recall(memory_session, user_id=user_id, query="Global fact")
        assert any(m.content == "Global fact" for m in results)

    def test_agent_scoped_visibility(self, memory_session: Session, user_id: int):
        from app.memory.service import recall, remember

        remember(
            memory_session,
            user_id=user_id,
            content="Agent-specific knowledge",
            scope=SCOPE_AGENT,
            agent_id=42,
            source="user_explicit",
        )
        # Should NOT be visible without matching agent_id.
        results = recall(memory_session, user_id=user_id, query="Agent-specific")
        # The fallback retrieval includes global scope only, so agent memory
        # shouldn't appear unless agent_id matches.
        agent_results = [m for m in results if m.scope == SCOPE_AGENT]
        assert len(agent_results) == 0

        # Should be visible with matching agent_id.
        results_with_agent = recall(
            memory_session, user_id=user_id, query="Agent-specific", agent_id=42
        )
        agent_results = [m for m in results_with_agent if m.scope == SCOPE_AGENT]
        assert len(agent_results) == 1

    def test_project_level_visibility(self, memory_session: Session, user_id: int):
        """Conversation-scoped memories are visible across the same project.

        A memory saved in one conversation (with a working directory) should be
        retrievable from another conversation that shares the same working
        directory (project), but not from a conversation in a different project.
        """
        from app.memory.service import recall, remember
        from app.models.conversation import Conversation

        # Create two conversations in the same project + one in a different project.
        conv_a = Conversation(user_id=user_id, working_directory="/proj/alpha")
        conv_b = Conversation(user_id=user_id, working_directory="/proj/alpha")
        conv_other = Conversation(user_id=user_id, working_directory="/proj/beta")
        for c in (conv_a, conv_b, conv_other):
            memory_session.add(c)
        memory_session.commit()
        for c in (conv_a, conv_b, conv_other):
            memory_session.refresh(c)

        # Save a project memory in conversation A (conversation scope).
        remember(
            memory_session,
            user_id=user_id,
            content="This project uses FastAPI and pytest",
            scope="conversation",
            conversation_id=conv_a.id,
            source="user_explicit",
        )

        # Visible from conversation B (same project / working directory).
        results_b = recall(
            memory_session, user_id=user_id, query="FastAPI pytest", conversation_id=conv_b.id
        )
        assert any("FastAPI" in m.content for m in results_b)

        # NOT visible from a conversation in a different project.
        results_other = recall(
            memory_session, user_id=user_id, query="FastAPI pytest", conversation_id=conv_other.id
        )
        assert not any("FastAPI" in m.content for m in results_other)


# --- Episodes ---


class TestEpisodes:
    def test_create_episode(self, memory_session: Session, user_id: int):
        from app.memory.service import create_episode

        episode = create_episode(
            memory_session,
            user_id=user_id,
            title="Fixed CI pipeline",
            summary="The CI was failing due to missing env var. Added it to .env.example.",
            outcome="success",
            importance=0.7,
            tags=["ci", "debugging"],
        )
        assert episode.id is not None
        assert episode.title == "Fixed CI pipeline"
        assert episode.outcome == "success"

    def test_list_episodes(self, memory_session: Session, user_id: int):
        from app.memory.service import create_episode, list_episodes

        create_episode(
            memory_session, user_id=user_id, title="Episode 1", summary="First"
        )
        create_episode(
            memory_session, user_id=user_id, title="Episode 2", summary="Second"
        )

        episodes = list_episodes(memory_session, user_id=user_id)
        assert len(episodes) == 2


# --- Working Memory ---


class TestWorkingMemory:
    def test_get_or_create(self, memory_session: Session):
        from app.memory.service import get_or_create_working_memory

        wm = get_or_create_working_memory(memory_session, conversation_id=1)
        assert wm.id is not None
        assert wm.conversation_id == 1
        assert wm.state == {}

    def test_update_state(self, memory_session: Session):
        from app.memory.service import (
            get_working_memory,
            update_working_memory_state,
        )

        update_working_memory_state(memory_session, 1, "current_goal", "Fix the bug")
        update_working_memory_state(memory_session, 1, "hypotheses", ["Missing import"])

        wm = get_working_memory(memory_session, 1)
        assert wm is not None
        assert wm.state["current_goal"] == "Fix the bug"
        assert wm.state["hypotheses"] == ["Missing import"]

    def test_update_summary(self, memory_session: Session):
        from app.memory.service import (
            get_working_memory,
            update_working_memory_summary,
        )

        update_working_memory_summary(
            memory_session, 1, "User was debugging a test failure.", up_to_message_id=10
        )

        wm = get_working_memory(memory_session, 1)
        assert wm is not None
        assert wm.summary == "User was debugging a test failure."
        assert wm.summary_up_to_message_id == 10


# --- Lifecycle ---


class TestLifecycle:
    def test_ttl_sweep_archives_expired(self, memory_session: Session, user_id: int):
        from app.memory.lifecycle import run_ttl_sweep
        from app.memory.service import get_memory, remember

        # Create a memory with 1-day TTL.
        memory = remember(
            memory_session,
            user_id=user_id,
            content="Temporary fact",
            ttl_days=1,
            source="user_explicit",
        )
        # Manually set valid_from to 2 days ago (naive datetime for SQLite).
        memory.valid_from = datetime.now() - timedelta(days=2)
        memory_session.add(memory)
        memory_session.commit()
        memory_session.refresh(memory)

        archived_count = run_ttl_sweep(memory_session, user_id=user_id)
        assert archived_count == 1

        updated = get_memory(memory_session, memory.id)
        assert updated.status == MEMORY_STATUS_ARCHIVED

    def test_decay_sweep_archives_low_importance(self, memory_session: Session, user_id: int):
        from app.memory.lifecycle import run_decay_sweep
        from app.memory.service import get_memory, remember

        # Create a low-importance memory that hasn't been accessed in a long time.
        memory = remember(
            memory_session,
            user_id=user_id,
            content="Old unimportant fact",
            importance=0.15,
            confidence=0.5,
            source="user_explicit",
        )
        # Set created_at to 100 days ago (well past decay threshold).
        memory.created_at = datetime.now(UTC) - timedelta(days=100)
        memory_session.add(memory)
        memory_session.commit()

        archived_count = run_decay_sweep(memory_session, user_id=user_id)
        assert archived_count == 1

        updated = get_memory(memory_session, memory.id)
        assert updated.status == MEMORY_STATUS_ARCHIVED

    def test_decay_preserves_preferences(self, memory_session: Session, user_id: int):
        from app.memory.lifecycle import run_decay_sweep
        from app.memory.service import get_memory, remember

        # Preferences should never be auto-archived by decay.
        memory = remember(
            memory_session,
            user_id=user_id,
            content="User prefers concise answers",
            memory_type=MEMORY_TYPE_PREFERENCE,
            importance=0.1,
            source="user_explicit",
        )
        memory.created_at = datetime.now(UTC) - timedelta(days=100)
        memory_session.add(memory)
        memory_session.commit()

        archived_count = run_decay_sweep(memory_session, user_id=user_id)
        assert archived_count == 0

        updated = get_memory(memory_session, memory.id)
        assert updated.status == MEMORY_STATUS_ACTIVE


# --- Context Builder ---


class TestContextBuilder:
    def test_build_memory_context_with_preferences(self, memory_session: Session, user_id: int):
        from app.memory.context_builder import build_memory_context
        from app.memory.service import remember

        remember(
            memory_session,
            user_id=user_id,
            content="Answer in Russian",
            memory_type=MEMORY_TYPE_PREFERENCE,
            importance=0.9,
            source="user_explicit",
        )

        context = build_memory_context(
            memory_session, user_id=user_id, conversation_id=1
        )
        assert context is not None
        assert "USER PREFERENCES" in context
        assert "Answer in Russian" in context

    def test_build_memory_context_empty(self, memory_session: Session, user_id: int):
        from app.memory.context_builder import build_memory_context

        # No memories stored — should return None.
        context = build_memory_context(
            memory_session, user_id=user_id, conversation_id=999
        )
        assert context is None

    def test_build_memory_context_with_working_memory(
        self, memory_session: Session, user_id: int
    ):
        from app.memory.context_builder import build_memory_context
        from app.memory.service import update_working_memory_state

        update_working_memory_state(memory_session, 1, "current_goal", "Fix auth bug")

        context = build_memory_context(
            memory_session, user_id=user_id, conversation_id=1
        )
        assert context is not None
        assert "WORKING MEMORY" in context
        assert "Fix auth bug" in context


# --- API endpoints ---


class TestMemoryAPI:
    @pytest.fixture(autouse=True)
    def setup_db(self):
        """Ensure memory tables exist in the app's test database."""
        from app.core.db import engine

        SQLModel.metadata.create_all(engine)
        yield

    @pytest.fixture
    def client(self):
        from fastapi.testclient import TestClient

        from app.main import app

        return TestClient(app)

    def test_list_memories_empty(self, client):
        resp = client.get("/api/memory")
        assert resp.status_code == 200
        assert resp.json() == []

    def test_create_and_get_memory(self, client):
        # Create.
        resp = client.post(
            "/api/memory",
            json={
                "content": "Test memory via API",
                "memory_type": "semantic",
                "importance": 0.7,
            },
        )
        assert resp.status_code == 201
        data = resp.json()
        assert data["content"] == "Test memory via API"
        memory_id = data["id"]

        # Get.
        resp = client.get(f"/api/memory/{memory_id}")
        assert resp.status_code == 200
        assert resp.json()["id"] == memory_id

    def test_update_memory(self, client):
        # Create.
        resp = client.post("/api/memory", json={"content": "Original"})
        memory_id = resp.json()["id"]

        # Update.
        resp = client.patch(f"/api/memory/{memory_id}", json={"content": "Updated"})
        assert resp.status_code == 200
        assert resp.json()["content"] == "Updated"

    def test_delete_memory(self, client):
        # Create.
        resp = client.post("/api/memory", json={"content": "To delete"})
        memory_id = resp.json()["id"]

        # Delete (archive).
        resp = client.delete(f"/api/memory/{memory_id}")
        assert resp.status_code == 204

        # Should be archived (not returned in active list).
        resp = client.get("/api/memory", params={"status": "active"})
        ids = [m["id"] for m in resp.json()]
        assert memory_id not in ids

    def test_memory_stats(self, client):
        # Create some memories.
        client.post("/api/memory", json={"content": "Fact 1", "memory_type": "semantic"})
        client.post("/api/memory", json={"content": "Pref 1", "memory_type": "preference"})

        resp = client.get("/api/memory/stats")
        assert resp.status_code == 200
        stats = resp.json()
        assert stats["total_active"] >= 2
        assert "semantic" in stats["by_type"]

    def test_invalid_memory_type(self, client):
        resp = client.post("/api/memory", json={"content": "Bad", "memory_type": "invalid"})
        assert resp.status_code == 422


# --- Фаза 3a gap-closing: confirmation workflow, pin, export, explain, entities ---


class TestConfirmationWorkflow:
    """Agent-extracted memories land in pending_confirmation until reviewed."""

    def test_agent_source_is_pending(self, memory_session: Session, user_id: int):
        from app.memory.service import remember

        memory = remember(
            memory_session,
            user_id=user_id,
            content="User likes dark mode",
            source="agent_extraction",
        )
        assert memory.status == "pending_confirmation"

    def test_user_explicit_is_active(self, memory_session: Session, user_id: int):
        from app.memory.service import remember

        memory = remember(
            memory_session,
            user_id=user_id,
            content="User likes dark mode",
            source="user_explicit",
        )
        assert memory.status == "active"

    def test_confirmed_flag_forces_active(self, memory_session: Session, user_id: int):
        from app.memory.service import remember

        memory = remember(
            memory_session,
            user_id=user_id,
            content="User likes dark mode",
            source="agent",
            confirmed=True,
        )
        assert memory.status == "active"

    def test_pending_excluded_from_recall(self, memory_session: Session, user_id: int):
        from app.memory.service import recall, remember

        # An agent-extracted fact lands pending; a user-confirmed fact is active.
        remember(
            memory_session,
            user_id=user_id,
            content="The deploy token is abc",
            memory_type=MEMORY_TYPE_SEMANTIC,
            source="agent_extraction",
        )
        results = recall(memory_session, user_id=user_id, query="deploy token")
        contents = [m.content for m in results]
        assert "The deploy token is abc" not in contents

    def test_confirm_promotes_to_active_and_recallable(
        self, memory_session: Session, user_id: int
    ):
        from app.memory.service import confirm_memory, recall, remember

        memory = remember(
            memory_session,
            user_id=user_id,
            content="Project uses FastAPI",
            memory_type=MEMORY_TYPE_SEMANTIC,
            source="agent_extraction",
        )
        confirmed = confirm_memory(memory_session, memory.id)
        assert confirmed.status == "active"

        results = recall(memory_session, user_id=user_id, query="FastAPI")
        contents = [m.content for m in results]
        assert "Project uses FastAPI" in contents

    def test_reject_archives(self, memory_session: Session, user_id: int):
        from app.memory.service import reject_memory, remember

        memory = remember(
            memory_session,
            user_id=user_id,
            content="Wrong guess",
            source="agent",
        )
        assert reject_memory(memory_session, memory.id) is True
        from app.memory.service import get_memory

        assert get_memory(memory_session, memory.id).status == "archived"

    def test_dedup_does_not_overwrite_confirmed(self, memory_session: Session, user_id: int):
        """A pending write must not overwrite a confirmed (active) fact."""
        from app.memory.service import get_memory, remember

        # Confirmed fact.
        confirmed = remember(
            memory_session,
            user_id=user_id,
            content="Project uses FastAPI",
            memory_type=MEMORY_TYPE_SEMANTIC,
            source="user_explicit",
        )
        # Pending near-duplicate from the agent.
        remember(
            memory_session,
            user_id=user_id,
            content="Project uses FastAPI",
            memory_type=MEMORY_TYPE_SEMANTIC,
            source="agent",
        )
        # The confirmed fact's content must be unchanged and still active.
        reloaded = get_memory(memory_session, confirmed.id)
        assert reloaded.status == "active"

    def test_list_pending(self, memory_session: Session, user_id: int):
        from app.memory.service import list_pending, remember

        remember(memory_session, user_id=user_id, content="Pending 1", source="agent")
        remember(memory_session, user_id=user_id, content="Pending 2", source="agent")
        pending = list_pending(memory_session, user_id=user_id)
        assert len(pending) == 2
        assert all(m.status == "pending_confirmation" for m in pending)


class TestPinAndExport:
    def test_pin_protects_from_decay(self, memory_session: Session, user_id: int):
        from app.memory.lifecycle import run_decay_sweep
        from app.memory.service import pin_memory, remember

        # A low-importance memory that WOULD be decayed.
        memory = remember(
            memory_session,
            user_id=user_id,
            content="Ephemeral note",
            memory_type=MEMORY_TYPE_SEMANTIC,
            importance=0.05,
            confidence=0.3,
            source="user_explicit",
        )
        pin_memory(memory_session, memory.id, pinned=True)
        # Even after a decay sweep, the pinned memory stays active.
        archived = run_decay_sweep(memory_session, user_id=user_id)
        assert archived == 0
        from app.memory.service import get_memory

        assert get_memory(memory_session, memory.id).status == "active"

    def test_unpin_allows_decay(self, memory_session: Session, user_id: int):
        from app.memory.lifecycle import run_decay_sweep
        from app.memory.service import get_memory, pin_memory, remember

        memory = remember(
            memory_session,
            user_id=user_id,
            content="Ephemeral note",
            memory_type=MEMORY_TYPE_SEMANTIC,
            importance=0.05,
            confidence=0.3,
            source="user_explicit",
        )
        pin_memory(memory_session, memory.id, pinned=True)
        pin_memory(memory_session, memory.id, pinned=False)
        archived = run_decay_sweep(memory_session, user_id=user_id)
        assert archived == 1
        assert get_memory(memory_session, memory.id).status == "archived"

    def test_export_json(self, memory_session: Session, user_id: int):
        import json

        from app.memory.service import export_memories, remember

        remember(
            memory_session,
            user_id=user_id,
            content="Fact for export",
            source="user_explicit",
        )
        data = export_memories(memory_session, user_id=user_id, fmt="json")
        payload = json.loads(data)
        assert payload["count"] >= 1
        assert any(m["content"] == "Fact for export" for m in payload["memories"])
        assert "pinned" in payload["memories"][0]

    def test_export_markdown(self, memory_session: Session, user_id: int):
        from app.memory.service import export_memories, remember

        remember(
            memory_session,
            user_id=user_id,
            content="Fact for export",
            source="user_explicit",
        )
        data = export_memories(memory_session, user_id=user_id, fmt="markdown")
        text = data.decode("utf-8")
        assert "# Memory export" in text
        assert "Fact for export" in text


class TestExplainability:
    def test_explain_returns_breakdown(self, memory_session: Session, user_id: int):
        from app.memory.service import explain_memory, remember

        memory = remember(
            memory_session,
            user_id=user_id,
            content="Explainable fact",
            source="user_explicit",
        )
        explanation = explain_memory(memory_session, memory.id)
        assert explanation is not None
        assert explanation["memory_id"] == memory.id
        score = explanation["score"]
        assert "total" in score
        assert "importance" in score
        assert "recency" in score
        assert "confidence" in score
        assert "type_priority" in score
        # Total equals the sum of the weighted components.
        assert abs(
            score["total"]
            - (score["importance"] + score["recency"] + score["confidence"] + score["type_priority"])
        ) < 1e-9

    def test_explain_returns_none_for_missing(self, memory_session: Session):
        from app.memory.service import explain_memory

        assert explain_memory(memory_session, 999999) is None

    def test_score_memory_consistent_with_legacy(
        self, memory_session: Session, user_id: int
    ):
        """The refactored score_memory yields the same total as ranking."""
        from datetime import UTC, datetime

        from app.memory.retrieval import _score_memory, score_memory
        from app.memory.service import remember

        memory = remember(
            memory_session,
            user_id=user_id,
            content="Scored fact",
            source="user_explicit",
        )
        now = datetime.now(UTC)
        assert score_memory(memory, now)["total"] == _score_memory(memory, now)


class TestEntities:
    def test_upsert_creates_then_merges(self, memory_session: Session, user_id: int):
        from app.memory.entities import upsert_entity

        e1 = upsert_entity(
            memory_session,
            user_id=user_id,
            name="FastAPI",
            entity_type="tool",
            aliases=["fast api"],
            attributes={"language": "python"},
        )
        e2 = upsert_entity(
            memory_session,
            user_id=user_id,
            name="FastAPI",
            entity_type="tool",
            aliases=["FastAPI framework"],
            description="Web framework",
        )
        assert e1.id == e2.id  # merged, not duplicated
        assert "fast api" in (e2.aliases or [])
        assert "FastAPI framework" in (e2.aliases or [])
        assert e2.description == "Web framework"

    def test_list_and_filter_by_query(self, memory_session: Session, user_id: int):
        from app.memory.entities import list_entities, upsert_entity

        upsert_entity(memory_session, user_id=user_id, name="Alice", entity_type="person")
        upsert_entity(
            memory_session, user_id=user_id, name="Postgres", entity_type="service"
        )
        # Query by name.
        people = list_entities(memory_session, user_id=user_id, query="Alice")
        assert len(people) == 1
        assert people[0].name == "Alice"
        # Query by alias.
        upsert_entity(
            memory_session, user_id=user_id, name="Bob", entity_type="person", aliases=["Robert"]
        )
        bob = list_entities(memory_session, user_id=user_id, query="Robert")
        assert len(bob) == 1
        assert bob[0].name == "Bob"

    def test_link_memory_to_entity_and_query_back(
        self, memory_session: Session, user_id: int
    ):
        from app.memory.entities import link_memory_to_entity, memories_for_entity, upsert_entity
        from app.memory.service import remember

        memory = remember(
            memory_session,
            user_id=user_id,
            content="FastAPI is used for the API",
            source="user_explicit",
        )
        entity = upsert_entity(memory_session, user_id=user_id, name="FastAPI", entity_type="tool")
        assert link_memory_to_entity(
            memory_session, memory_id=memory.id, entity_id=entity.id
        ) is True
        # Idempotent.
        assert link_memory_to_entity(
            memory_session, memory_id=memory.id, entity_id=entity.id
        ) is False
        linked = memories_for_entity(memory_session, entity.id)
        assert len(linked) == 1
        assert linked[0].content == "FastAPI is used for the API"

    def test_delete_entity_cascades_links(self, memory_session: Session, user_id: int):
        from app.memory.entities import (
            delete_entity,
            get_entity,
            link_memory_to_entity,
            upsert_entity,
        )
        from app.memory.service import remember

        memory = remember(
            memory_session, user_id=user_id, content="Note about X", source="user_explicit"
        )
        entity = upsert_entity(memory_session, user_id=user_id, name="X", entity_type="concept")
        link_memory_to_entity(memory_session, memory_id=memory.id, entity_id=entity.id)
        assert delete_entity(memory_session, entity.id) is True
        assert get_entity(memory_session, entity.id) is None

    def test_extract_entities_from_text_upserts(self, memory_session: Session, user_id: int):
        from app.memory.entities import extract_entities_from_text, list_entities

        provider = _FakeProvider(
            '{"entities": [{"name": "Redis", "entity_type": "service", '
            '"aliases": ["redis-server"], "description": "cache"}]}'
        )
        import asyncio

        created = asyncio.run(
            extract_entities_from_text(
                memory_session,
                provider=provider,
                model="test",
                user_id=user_id,
                text="We deployed Redis for caching.",
            )
        )
        assert len(created) == 1
        assert created[0].name == "Redis"
        # Persisted.
        found = list_entities(memory_session, user_id=user_id, query="Redis")
        assert len(found) == 1


class TestMemoryAPIExtended:
    """New endpoints: confirm/reject/pin/explain/export + entities CRUD."""

    @pytest.fixture(autouse=True)
    def setup_db(self):
        from app.core.db import engine

        SQLModel.metadata.create_all(engine)
        yield

    @pytest.fixture
    def client(self):
        from fastapi.testclient import TestClient

        from app.main import app

        return TestClient(app)

    def test_pending_confirm_reject_flow(self, client):
        # An agent-sourced memory via the API would need source override; the API
        # always stores user_explicit (active). So simulate pending by creating
        # through the service directly, then exercising the endpoints.
        from sqlmodel import Session

        from app.core.db import engine
        from app.memory.service import remember

        with Session(engine) as session:
            memory = remember(
                session, user_id=1, content="Pending API fact", source="agent"
            )
            mem_id = memory.id

        # Confirm endpoint.
        resp = client.post(f"/api/memory/{mem_id}/confirm")
        assert resp.status_code == 200
        assert resp.json()["status"] == "active"

        # A second pending one to reject.
        with Session(engine) as session:
            mem2 = remember(session, user_id=1, content="Pending API fact 2", source="agent")
            mem2_id = mem2.id
        resp = client.post(f"/api/memory/{mem2_id}/reject")
        assert resp.status_code == 204

    def test_pin_endpoint(self, client):
        create = client.post("/api/memory", json={"content": "Pin me"})
        mem_id = create.json()["id"]
        resp = client.post(f"/api/memory/{mem_id}/pin", json={"pinned": True})
        assert resp.status_code == 200
        assert resp.json()["pinned"] is True
        resp = client.post(f"/api/memory/{mem_id}/pin", json={"pinned": False})
        assert resp.json()["pinned"] is False

    def test_explain_endpoint(self, client):
        create = client.post("/api/memory", json={"content": "Explain me"})
        mem_id = create.json()["id"]
        resp = client.get(f"/api/memory/{mem_id}/explain")
        assert resp.status_code == 200
        body = resp.json()
        assert body["memory_id"] == mem_id
        assert "total" in body["score"]

    def test_export_json_endpoint(self, client):
        client.post("/api/memory", json={"content": "Exportable fact"})
        resp = client.get("/api/memory/export", params={"format": "json"})
        assert resp.status_code == 200
        assert "memories" in resp.json()

    def test_export_markdown_endpoint(self, client):
        resp = client.get("/api/memory/export", params={"format": "markdown"})
        assert resp.status_code == 200
        assert "# Memory export" in resp.text

    def test_stats_include_pending_and_entities(self, client):
        resp = client.get("/api/memory/stats")
        body = resp.json()
        assert "total_pending" in body
        assert "total_entities" in body

    def test_entities_crud(self, client):
        # Create.
        resp = client.post(
            "/api/entities",
            json={"name": "MyService", "entity_type": "service", "aliases": ["svc"]},
        )
        assert resp.status_code == 201
        entity_id = resp.json()["id"]
        # List.
        resp = client.get("/api/entities", params={"query": "MyService"})
        assert resp.status_code == 200
        assert any(e["id"] == entity_id for e in resp.json())
        # Update.
        resp = client.patch(
            f"/api/entities/{entity_id}", json={"description": "Updated desc"}
        )
        assert resp.status_code == 200
        assert resp.json()["description"] == "Updated desc"
        # Delete.
        resp = client.delete(f"/api/entities/{entity_id}")
        assert resp.status_code == 204
        # Get -> 404.
        resp = client.get(f"/api/entities/{entity_id}")
        assert resp.status_code == 404


# --- Test helpers ---


class _FakeProvider:
    """Minimal provider stub returning a fixed chat_completion content.

    Used only for entity-extraction unit tests (the real test suite uses
    ScriptedProvider for streaming agent tests).
    """

    def __init__(self, content: str):
        self._content = content

    async def chat_completion(self, messages, *, model, **kwargs):  # type: ignore[no-untyped-def]
        from app.providers.base import ChatResult

        return ChatResult(content=self._content)

