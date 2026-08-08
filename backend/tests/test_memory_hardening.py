"""Tests for the Phase-3a memory hardening: auto-extraction (A), hybrid search
and Cyrillic-safe FTS5 (B), consolidation/entity-linking/conflicts (C).

The ``hybrid_engine`` fixture recreates the production schema the way
migration 0011 + 0021 build it: FTS5 mirror + triggers + vec0 virtual table,
so FTS and vector legs are both exercised without SQLModel.create_all limits.
"""

from __future__ import annotations

import json

import pytest
from sqlalchemy import event, text
from sqlmodel import Session, SQLModel, create_engine, select

from app.memory.models import (
    MEMORY_STATUS_ACTIVE,
    MEMORY_STATUS_PENDING_CONFIRMATION,
    MEMORY_STATUS_SUPERSEDED,
)

VEC_TABLE_DDL = (
    "CREATE VIRTUAL TABLE IF NOT EXISTS memory_vec USING vec0("
    "memory_id INTEGER PRIMARY KEY, embedding FLOAT[3] distance_metric=cosine)"
)

FTS_DDL = (
    "CREATE VIRTUAL TABLE IF NOT EXISTS memory_fts USING fts5("
    "content, tags, memory_type UNINDEXED, content='memory_items', content_rowid='id')"
)
FTS_TRIGGERS = [
    (
        "memory_items_ai",
        """CREATE TRIGGER IF NOT EXISTS memory_items_ai AFTER INSERT ON memory_items BEGIN
            INSERT INTO memory_fts(rowid, content, tags, memory_type)
            VALUES (new.id, new.content, COALESCE(new.tags, ''), new.memory_type);
        END""",
    ),
    (
        "memory_items_ad",
        """CREATE TRIGGER IF NOT EXISTS memory_items_ad AFTER DELETE ON memory_items BEGIN
            INSERT INTO memory_fts(memory_fts, rowid, content, tags, memory_type)
            VALUES ('delete', old.id, old.content, COALESCE(old.tags, ''), old.memory_type);
        END""",
    ),
    (
        "memory_items_au",
        """CREATE TRIGGER IF NOT EXISTS memory_items_au AFTER UPDATE ON memory_items BEGIN
            INSERT INTO memory_fts(memory_fts, rowid, content, tags, memory_type)
            VALUES ('delete', old.id, old.content, COALESCE(old.tags, ''), old.memory_type);
            INSERT INTO memory_fts(rowid, content, tags, memory_type)
            VALUES (new.id, new.content, COALESCE(new.tags, ''), new.memory_type);
        END""",
    ),
]


@pytest.fixture
def hybrid_engine():
    """Engine with the full production memory schema (FTS5 + vec0)."""
    engine = create_engine("sqlite://", echo=False)

    try:
        import sqlite_vec

        @event.listens_for(engine, "connect")
        def _load_vec(dbapi_conn, connection_record):
            dbapi_conn.enable_load_extension(True)
            sqlite_vec.load(dbapi_conn)
            dbapi_conn.enable_load_extension(False)
    except Exception:
        pass

    SQLModel.metadata.create_all(engine)
    with engine.begin() as conn:
        conn.execute(text(FTS_DDL))
        for _, trigger in FTS_TRIGGERS:
            conn.execute(text(trigger))
        conn.execute(text(VEC_TABLE_DDL))
    with Session(engine) as session:
        yield session


@pytest.fixture
def user_id(hybrid_engine: Session) -> int:
    from app.models.user import User

    user = User(username="test", display_name="Test User")
    hybrid_engine.add(user)
    hybrid_engine.commit()
    hybrid_engine.refresh(user)
    return user.id


class FakeProvider:
    """Scripted LLM provider: yields canned responses per call."""

    def __init__(self, responses: list[str]) -> None:
        self.responses = list(responses)
        self.calls = 0

    async def chat_completion(self, messages, **kwargs):
        self.calls += 1
        raw = self.responses.pop(0)
        return SimpleNamespace(content=raw)

    async def embed(self, texts, *, model=None):
        raise NotImplementedError("not used in these tests")


class SimpleNamespace:
    def __init__(self, **kw) -> None:
        self.__dict__.update(kw)


# --- B1: Cyrillic + FTS5 injection safety ---


class TestCyrillicFts:
    def test_recall_finds_russian_memory(self, hybrid_engine: Session, user_id: int):
        from app.memory.service import recall, remember

        remember(
            hybrid_engine,
            user_id=user_id,
            content="Проект использует FastAPI и SQLite для хранения данных",
            source="user_explicit",
        )
        results = recall(hybrid_engine, user_id=user_id, query="какая база данных проекта")
        assert any("SQLite" in m.content for m in results)

    def test_dedup_handles_fts_query_syntax(self, hybrid_engine: Session, user_id: int):
        """Content containing FTS5 operators must not break dedup."""
        from app.memory.service import remember

        m1 = remember(
            hybrid_engine,
            user_id=user_id,
            content='Project uses "NEAR" queries and OR logic in search',
            source="user_explicit",
        )
        m2 = remember(
            hybrid_engine,
            user_id=user_id,
            content='Project uses "NEAR" queries and OR logic in search',
            source="user_explicit",
        )
        assert m1.id == m2.id

    def test_recall_with_special_chars_does_not_crash(self, hybrid_engine: Session, user_id: int):
        from app.memory.service import recall, remember

        remember(
            hybrid_engine,
            user_id=user_id,
            content="Deploy script: run python -m build --release",
            source="user_explicit",
        )
        results = recall(hybrid_engine, user_id=user_id, query='deploy "release" -build')
        assert len(results) >= 1


# --- B2: hybrid vector recall ---


class TestHybridRetrieval:
    def test_vector_leg_finds_semantic_match(self, hybrid_engine: Session, user_id: int):
        from app.memory.embeddings import upsert_vector
        from app.memory.service import recall, remember

        mem = remember(
            hybrid_engine,
            user_id=user_id,
            content="Deployment uses Docker Compose",
            source="user_explicit",
        )
        # Hand-crafted vectors: the query embedding is closest to this memory.
        assert upsert_vector(
            hybrid_engine, memory_id=mem.id, embedding=[0.1, 0.2, 0.3], model="test"
        )
        results = recall(
            hybrid_engine,
            user_id=user_id,
            query="Docker Compose",
            query_embedding=[0.11, 0.21, 0.31],
        )
        assert any(m.id == mem.id for m in results)

    def test_vector_leg_skipped_without_query_embedding(self, hybrid_engine: Session, user_id: int):
        from app.memory.embeddings import upsert_vector
        from app.memory.service import recall, remember

        mem = remember(
            hybrid_engine, user_id=user_id, content="Some fact here", source="user_explicit"
        )
        assert upsert_vector(
            hybrid_engine, memory_id=mem.id, embedding=[0.9, 0.1, 0.2], model="test"
        )
        # No query_embedding → no crash, FTS leg only.
        results = recall(hybrid_engine, user_id=user_id, query="some fact")
        assert any(m.id == mem.id for m in results)

    def test_include_preferences_false_excludes_prefs(self, hybrid_engine: Session, user_id: int):
        from app.memory.service import recall, remember

        remember(
            hybrid_engine,
            user_id=user_id,
            content="User likes short answers",
            memory_type="preference",
            source="user_explicit",
        )
        fact = remember(
            hybrid_engine,
            user_id=user_id,
            content="Project uses FastAPI",
            source="user_explicit",
        )
        with_prefs = recall(
            hybrid_engine,
            user_id=user_id,
            query="fastapi",
            include_preferences=True,
        )
        without_prefs = recall(
            hybrid_engine,
            user_id=user_id,
            query="fastapi",
            include_preferences=False,
        )
        assert any(m.id == fact.id for m in without_prefs)
        assert all(m.memory_type != "preference" for m in without_prefs)
        assert any(m.memory_type == "preference" for m in with_prefs)

    def test_hard_delete_removes_vector(self, hybrid_engine: Session, user_id: int):
        from app.memory.embeddings import search_vectors, upsert_vector
        from app.memory.service import forget, remember

        mem = remember(
            hybrid_engine, user_id=user_id, content="Ephemeral fact", source="user_explicit"
        )
        assert upsert_vector(
            hybrid_engine, memory_id=mem.id, embedding=[0.5, 0.5, 0.5], model="test"
        )
        assert forget(hybrid_engine, mem.id, hard=True)
        hits = search_vectors(hybrid_engine, embedding=[0.5, 0.5, 0.5], limit=10)
        assert all(mid != mem.id for mid, _ in hits)


# --- C3: contradiction handling ---


class TestConflictSupersede:
    def test_agent_correction_sets_supersedes_id(self, hybrid_engine: Session, user_id: int):
        from app.memory.service import remember

        old = remember(
            hybrid_engine,
            user_id=user_id,
            content="User prefers Python for backend",
            memory_type="preference",
            source="user_explicit",
        )
        new = remember(
            hybrid_engine,
            user_id=user_id,
            content="User prefers Python over Java",
            memory_type="preference",
            importance=0.85,
            source="agent",
        )
        assert new.id != old.id
        assert new.supersedes_id == old.id
        assert new.status == MEMORY_STATUS_PENDING_CONFIRMATION

    def test_confirm_archives_superseded_memory(self, hybrid_engine: Session, user_id: int):
        from app.memory.service import confirm_memory, get_memory, remember

        old = remember(
            hybrid_engine,
            user_id=user_id,
            content="Deploy uses Docker for backend",
            source="user_explicit",
        )
        new = remember(
            hybrid_engine,
            user_id=user_id,
            content="Deploy uses Podman for backend now",
            importance=0.85,
            source="agent",
        )
        assert new.supersedes_id == old.id
        confirmed = confirm_memory(hybrid_engine, new.id)
        assert confirmed.status == MEMORY_STATUS_ACTIVE
        old_after = get_memory(hybrid_engine, old.id)
        assert old_after.status == MEMORY_STATUS_SUPERSEDED
        assert old_after.supersedes_id == new.id

    async def test_llm_conflict_check_sets_supersedes(
        self, hybrid_engine: Session, user_id: int
    ) -> None:
        from app.memory.extractor import detect_conflicts_with_active
        from app.memory.service import remember

        old = remember(
            hybrid_engine,
            user_id=user_id,
            content="Server is deployed to staging",
            source="user_explicit",
        )
        provider = FakeProvider(
            [
                json.dumps(
                    {"results": [{"index": 0, "conflict_with": old.id, "kind": "contradicts"}]}
                )
            ]
        )
        extracted = {
            "user_preferences": [],
            "project_facts": [{"content": "Server is deployed to production"}],
            "procedures": [],
        }
        result = await detect_conflicts_with_active(
            hybrid_engine,
            provider=provider,
            model="test",
            user_id=user_id,
            extracted=extracted,
        )
        assert result["fact"]["Server is deployed to production"] == old.id


# --- C2: entity linking + entity-driven recall ---


class TestEntityLinking:
    async def test_extraction_links_entities_to_anchor_memory(
        self, hybrid_engine: Session, user_id: int
    ):
        from app.memory.entities import extract_entities_from_text, memories_for_entity
        from app.memory.service import remember

        anchor = remember(
            hybrid_engine, user_id=user_id, content="Project uses FastAPI", source="user_explicit"
        )
        provider = FakeProvider(
            [
                json.dumps(
                    {
                        "entities": [
                            {
                                "name": "FastAPI",
                                "entity_type": "tool",
                                "aliases": ["fastapi"],
                                "description": "Web framework",
                            }
                        ]
                    }
                )
            ]
        )
        entities = await extract_entities_from_text(
            hybrid_engine,
            provider=provider,
            model="test",
            user_id=user_id,
            text="Project uses FastAPI",
            link_memory_id=anchor.id,
        )
        assert len(entities) == 1
        linked = memories_for_entity(hybrid_engine, entity_id=entities[0].id, active_only=True)
        assert any(m.id == anchor.id for m in linked)

    async def test_entity_search_returns_linked_memory(self, hybrid_engine: Session, user_id: int):
        from app.memory.entities import extract_entities_from_text, link_memory_to_entity
        from app.memory.service import recall, remember

        anchor = remember(
            hybrid_engine,
            user_id=user_id,
            content="The auth service uses JWT",
            source="user_explicit",
        )
        provider = FakeProvider(
            [
                json.dumps(
                    {
                        "entities": [
                            {
                                "name": "AuthService",
                                "entity_type": "service",
                                "description": "Authentication service",
                            }
                        ]
                    }
                )
            ]
        )
        entities = await extract_entities_from_text(
            hybrid_engine,
            provider=provider,
            model="test",
            user_id=user_id,
            text="AuthService handles auth",
            link_memory_id=None,
        )
        assert link_memory_to_entity(hybrid_engine, memory_id=anchor.id, entity_id=entities[0].id)
        results = recall(hybrid_engine, user_id=user_id, query="AuthService what does it do")
        assert any(m.id == anchor.id for m in results)


# --- C1: consolidation ---


class TestConsolidation:
    async def test_sweep_merges_group_without_llm(self, hybrid_engine: Session, user_id: int):
        from app.memory.lifecycle import run_consolidation_sweep
        from app.memory.models import MemoryItem
        from app.memory.service import list_memories, remember

        # Word overlaps are below the 0.7 dedup threshold, so all three stay
        # distinct — but they share a tag, so consolidation groups them.
        for content in (
            "Deploy uses docker compose up",
            "Deploy command is compose based",
            "Deploy stack runs docker services",
        ):
            remember(
                hybrid_engine,
                user_id=user_id,
                content=content,
                tags=["deploy"],
                source="user_explicit",
            )
        results = await run_consolidation_sweep(hybrid_engine, user_id=user_id)
        assert results["consolidated"] >= 1
        active = list_memories(hybrid_engine, user_id=user_id)
        superseded = hybrid_engine.exec(
            select(MemoryItem).where(MemoryItem.status == MEMORY_STATUS_SUPERSEDED)
        ).all()
        assert len(active) == 1
        assert len(superseded) >= 2

    async def test_sweep_with_llm_merge(self, hybrid_engine: Session, user_id: int):
        from app.memory.lifecycle import run_consolidation_sweep
        from app.memory.service import list_memories, remember

        for content in (
            "Deploy uses docker compose up",
            "Deploy command is compose based",
            "Deploy stack runs docker services",
        ):
            remember(
                hybrid_engine,
                user_id=user_id,
                content=content,
                tags=["deploy"],
                source="user_explicit",
            )
        provider = FakeProvider(
            [
                json.dumps(
                    {
                        "content": "Deploy via docker compose up",
                        "importance": 0.8,
                        "confidence": 0.9,
                    }
                )
            ]
        )
        results = await run_consolidation_sweep(
            hybrid_engine, provider=provider, model="test", user_id=user_id
        )
        assert results["consolidated"] == 1
        active = list_memories(hybrid_engine, user_id=user_id)
        assert active[0].content == "Deploy via docker compose up"


# --- A: auto-extraction hook ---


class TestAutoExtraction:
    def test_should_extract_gate(self):
        from app.memory.extraction_runner import _should_extract

        assert _should_extract(10, 0, min_interval=8) is True
        assert _should_extract(5, 0, min_interval=8) is False
        # Tool-heavy turns bypass the message gate.
        assert _should_extract(2, 3, min_interval=8) is True

    async def test_maybe_extract_after_run_creates_pending_and_debounces(self, monkeypatch):
        """End-to-end: messages → extraction → pending memories + pointer set;
        second call with no new messages is skipped."""
        from sqlalchemy import text as sa_text

        from app.agent.service import append_message, create_conversation
        from app.core.db import engine, init_db
        from app.memory.extraction_runner import maybe_extract_after_run

        init_db()

        # Ensure the vec/FTS mirrors exist for this engine too.
        with engine.begin() as conn:
            conn.execute(sa_text(FTS_DDL))
            for _, trigger in FTS_TRIGGERS:
                conn.execute(sa_text(trigger))

        from app.memory.service import list_pending

        with Session(engine) as session:
            conv = create_conversation(
                session, user_id=1, title="Auto-extract test", working_directory=None
            )
            conv_id = conv.id
            for i in range(8):
                append_message(session, conversation_id=conv_id, role="user", content=f"msg {i}")
            session.commit()

        extraction_payload = json.dumps(
            {
                "user_preferences": [
                    {"content": "User prefers concise answers", "importance": 0.8}
                ],
                "project_facts": [{"content": "Project uses FastAPI", "importance": 0.6}],
                "procedures": [],
                "episode_summary": {
                    "title": "Test run",
                    "summary": "Nothing much happened",
                    "outcome": "success",
                },
            }
        )
        provider = FakeProvider([extraction_payload])

        ran = await maybe_extract_after_run(
            conversation_id=conv_id, run_id=None, provider=provider, model="test"
        )
        assert ran is True

        with Session(engine) as session:
            pending = list_pending(session, user_id=1)
            assert any("FastAPI" in p.content for p in pending)
            from app.memory.service import get_working_memory

            wm = get_working_memory(session, conv_id)
            assert (wm.state or {}).get("last_extraction_message_id") is not None

        # Debounce: no new messages → second call skipped.
        provider2 = FakeProvider([])
        ran2 = await maybe_extract_after_run(
            conversation_id=conv_id, run_id=None, provider=provider2, model="test"
        )
        assert ran2 is False


# --- D1: dev-path bootstrap creates the FTS5/vec0 mirrors (M1) ---


class TestDevPath:
    def test_init_db_creates_memory_virtual_tables(self):
        """init_db() (dev path) must create memory_fts + triggers + memory_vec."""
        from app.core.db import VEC_AVAILABLE, engine, init_db

        # conftest may have already run init_db() on this engine — it is
        # idempotent, so a second call just verifies re-entry safety.
        init_db()

        with engine.connect() as conn:
            tables = {
                row[0]
                for row in conn.execute(
                    text("SELECT name FROM sqlite_master WHERE type='table'")
                )
            }
            assert "memory_fts" in tables
            triggers = {
                row[0]
                for row in conn.execute(
                    text("SELECT name FROM sqlite_master WHERE type='trigger'")
                )
            }
            assert "memory_items_ai" in triggers
            assert "memory_items_ad" in triggers
            assert "memory_items_au" in triggers
            # vec0 exists only where the sqlite-vec extension loads; without it
            # init_db logs a warning and degrades to FTS5-only retrieval.
            if VEC_AVAILABLE:
                assert "memory_vec" in tables

        if not VEC_AVAILABLE:
            return

        # sqlite-vec roundtrip: a 1536-dim vector must be accepted.
        with engine.begin() as conn:
            conn.execute(
                text("INSERT INTO memory_vec(memory_id, embedding) VALUES (:id, :emb)"),
                {"id": 999999, "emb": json.dumps([0.1] * 1536)},
            )
            row = conn.execute(
                text("SELECT memory_id FROM memory_vec WHERE memory_id = 999999")
            ).first()
            assert row is not None and row[0] == 999999
            conn.execute(text("DELETE FROM memory_vec WHERE memory_id = 999999"))


# --- D2: backfill gate — no provider calls without a vec0 table (M1) ---


class CountingProvider:
    """Embedding-only fake: counts embed() calls, returns fixed vectors."""

    def __init__(self) -> None:
        self.embed_calls = 0

    async def embed(self, texts, *, model=None):
        self.embed_calls += 1
        return [[0.1] * 1536 for _ in texts]


@pytest.fixture
def plain_memory_session():
    """In-memory session without the vec0 table (backfill-gate tests)."""
    engine = create_engine("sqlite://", echo=False)
    SQLModel.metadata.create_all(engine)
    with Session(engine) as session:
        yield session


@pytest.fixture
def plain_user_id(plain_memory_session: Session) -> int:
    from app.models.user import User

    user = User(username="test", display_name="Test User")
    plain_memory_session.add(user)
    plain_memory_session.commit()
    plain_memory_session.refresh(user)
    return user.id


class TestBackfillGate:
    def _seed(self, session: Session, user_id: int) -> None:
        from app.memory.service import remember

        remember(
            session, user_id=user_id, content="Alpha deployment fact", source="user_explicit"
        )
        remember(
            session, user_id=user_id, content="Beta packaging fact", source="user_explicit"
        )

    async def test_backfill_skips_provider_without_vec_table(
        self, plain_memory_session: Session, plain_user_id: int, monkeypatch
    ):
        """vec_table_ready()=False → no embed() calls, no backfill work."""
        from app.memory import embeddings as emb
        from app.memory.embeddings import backfill_embeddings

        self._seed(plain_memory_session, plain_user_id)
        monkeypatch.setattr(emb, "vec_table_ready", lambda: False)
        provider = CountingProvider()
        done = await backfill_embeddings(
            plain_memory_session, provider=provider, model="test", dimension=1536
        )
        assert done == 0
        assert provider.embed_calls == 0

    async def test_backfill_calls_provider_when_vec_table_ready(
        self, plain_memory_session: Session, plain_user_id: int, monkeypatch
    ):
        """vec_table_ready()=True → embed() called once per memory (upsert on
        the vec-less test engine still fails, but the gate passed)."""
        from app.memory import embeddings as emb
        from app.memory.embeddings import backfill_embeddings

        self._seed(plain_memory_session, plain_user_id)
        monkeypatch.setattr(emb, "vec_table_ready", lambda: True)
        provider = CountingProvider()
        await backfill_embeddings(
            plain_memory_session, provider=provider, model="test", dimension=1536
        )
        assert provider.embed_calls == 2
