"""Tests for agent profiles (Фаза 3a §2 — Multi-personality agents)."""

from __future__ import annotations

import pytest
from sqlmodel import Session, SQLModel, create_engine


@pytest.fixture
def profile_session():
    """Create an in-memory SQLite session with all tables."""
    engine = create_engine("sqlite://", echo=False)
    SQLModel.metadata.create_all(engine)
    with Session(engine) as session:
        yield session


# --- Service CRUD ---


class TestProfileService:
    def test_create_profile(self, profile_session: Session):
        from app.agent.personalities.service import create_profile, get_profile

        p = create_profile(
            profile_session,
            name="Test Profile",
            slug="test-profile",
            description="A test",
            system_prompt="You are a test agent.",
            avatar_color="#FF0000",
        )
        assert p.id is not None
        assert p.name == "Test Profile"
        assert p.slug == "test-profile"
        assert p.is_builtin is False
        assert p.is_active is True

        fetched = get_profile(profile_session, p.id)
        assert fetched is not None
        assert fetched.slug == "test-profile"

    def test_get_profile_by_slug(self, profile_session: Session):
        from app.agent.personalities.service import create_profile, get_profile_by_slug

        create_profile(profile_session, name="Coder", slug="coder")
        found = get_profile_by_slug(profile_session, "coder")
        assert found is not None
        assert found.name == "Coder"

        assert get_profile_by_slug(profile_session, "nonexistent") is None

    def test_list_profiles_excludes_inactive(self, profile_session: Session):
        from app.agent.personalities.service import create_profile, list_profiles

        create_profile(profile_session, name="Active", slug="active", is_active=True)
        create_profile(profile_session, name="Inactive", slug="inactive", is_active=False)

        active = list_profiles(profile_session)
        assert len(active) == 1
        assert active[0].slug == "active"

        all_profiles = list_profiles(profile_session, include_inactive=True)
        assert len(all_profiles) == 2

    def test_update_profile(self, profile_session: Session):
        from app.agent.personalities.service import create_profile, update_profile

        p = create_profile(profile_session, name="Old", slug="old")
        updated = update_profile(profile_session, p.id, name="New", avatar_color="#00FF00")
        assert updated is not None
        assert updated.name == "New"
        assert updated.avatar_color == "#00FF00"
        assert updated.slug == "old"  # unchanged

    def test_update_nonexistent_returns_none(self, profile_session: Session):
        from app.agent.personalities.service import update_profile

        assert update_profile(profile_session, 9999, name="X") is None

    def test_delete_profile(self, profile_session: Session):
        from app.agent.personalities.service import create_profile, delete_profile, get_profile

        p = create_profile(profile_session, name="ToDelete", slug="to-delete")
        assert delete_profile(profile_session, p.id) is True
        assert get_profile(profile_session, p.id) is None

    def test_delete_builtin_raises(self, profile_session: Session):
        from app.agent.personalities.service import create_profile, delete_profile

        p = create_profile(profile_session, name="Builtin", slug="builtin", is_builtin=True)
        with pytest.raises(ValueError, match="Cannot delete a built-in"):
            delete_profile(profile_session, p.id)

    def test_delete_nonexistent_returns_false(self, profile_session: Session):
        from app.agent.personalities.service import delete_profile

        assert delete_profile(profile_session, 9999) is False


# --- Preset seeding ---


class TestSeeding:
    def test_seed_creates_five_presets(self, profile_session: Session):
        from app.agent.personalities.seeding import seed_builtin_profiles
        from app.agent.personalities.service import list_profiles

        created = seed_builtin_profiles(profile_session)
        assert created == 5

        profiles = list_profiles(profile_session)
        slugs = {p.slug for p in profiles}
        assert slugs == {"assistant", "coder", "researcher", "writer", "dm"}

    def test_seed_is_idempotent(self, profile_session: Session):
        from app.agent.personalities.seeding import seed_builtin_profiles

        first = seed_builtin_profiles(profile_session)
        second = seed_builtin_profiles(profile_session)
        assert first == 5
        assert second == 0

    def test_seed_preserves_user_edits(self, profile_session: Session):
        from app.agent.personalities.seeding import seed_builtin_profiles
        from app.agent.personalities.service import get_profile_by_slug, update_profile

        seed_builtin_profiles(profile_session)
        coder = get_profile_by_slug(profile_session, "coder")
        assert coder is not None
        update_profile(profile_session, coder.id, name="Custom Coder")

        # Re-seed should not overwrite.
        seed_builtin_profiles(profile_session)
        coder = get_profile_by_slug(profile_session, "coder")
        assert coder.name == "Custom Coder"

    def test_presets_have_system_prompts(self, profile_session: Session):
        from app.agent.personalities.seeding import seed_builtin_profiles
        from app.agent.personalities.service import list_profiles

        seed_builtin_profiles(profile_session)
        for p in list_profiles(profile_session):
            assert p.system_prompt, f"{p.slug} missing system_prompt"
            assert p.avatar_color, f"{p.slug} missing avatar_color"
            assert p.is_builtin is True


# --- Conversation profile_id ---


class TestConversationProfile:
    def test_create_conversation_with_profile(self, profile_session: Session):
        from app.agent.personalities.service import create_profile
        from app.agent.service import create_conversation, get_or_create_default_user

        user = get_or_create_default_user(profile_session)
        p = create_profile(profile_session, name="Writer", slug="writer")

        conv = create_conversation(
            profile_session, user_id=user.id, title="Test", profile_id=p.id
        )
        assert conv.profile_id == p.id

    def test_update_conversation_profile(self, profile_session: Session):
        from app.agent.personalities.service import create_profile
        from app.agent.service import (
            create_conversation,
            get_or_create_default_user,
            update_conversation,
        )

        user = get_or_create_default_user(profile_session)
        p1 = create_profile(profile_session, name="A", slug="a")
        p2 = create_profile(profile_session, name="B", slug="b")

        conv = create_conversation(
            profile_session, user_id=user.id, title="Test", profile_id=p1.id
        )
        assert conv.profile_id == p1.id

        updated = update_conversation(profile_session, conv.id, profile_id=p2.id)
        assert updated is not None
        assert updated.profile_id == p2.id

    def test_clear_conversation_profile(self, profile_session: Session):
        from app.agent.personalities.service import create_profile
        from app.agent.service import (
            create_conversation,
            get_or_create_default_user,
            update_conversation,
        )

        user = get_or_create_default_user(profile_session)
        p = create_profile(profile_session, name="X", slug="x")

        conv = create_conversation(
            profile_session, user_id=user.id, title="Test", profile_id=p.id
        )
        # Sentinel -1 clears the profile.
        updated = update_conversation(profile_session, conv.id, profile_id=-1)
        assert updated is not None
        assert updated.profile_id is None


# --- API endpoints ---


class TestProfilesAPI:
    @pytest.fixture
    def client(self):
        from fastapi.testclient import TestClient

        from app.main import app

        return TestClient(app)

    def test_list_profiles(self, client):
        resp = client.get("/api/profiles")
        assert resp.status_code == 200
        data = resp.json()
        assert isinstance(data, list)
        # Seeded presets should be present.
        slugs = {p["slug"] for p in data}
        assert "assistant" in slugs
        assert "coder" in slugs

    def test_create_and_get_profile(self, client):
        resp = client.post(
            "/api/profiles",
            json={
                "name": "Custom",
                "slug": "custom-test",
                "description": "A custom profile",
                "system_prompt": "You are custom.",
                "avatar_color": "#123456",
            },
        )
        assert resp.status_code == 201
        data = resp.json()
        assert data["name"] == "Custom"
        assert data["slug"] == "custom-test"
        assert data["is_builtin"] is False

        # Fetch by id.
        resp2 = client.get(f"/api/profiles/{data['id']}")
        assert resp2.status_code == 200
        assert resp2.json()["slug"] == "custom-test"

    def test_create_duplicate_slug_409(self, client):
        client.post("/api/profiles", json={"name": "A", "slug": "dup-slug"})
        resp = client.post("/api/profiles", json={"name": "B", "slug": "dup-slug"})
        assert resp.status_code == 409

    def test_update_profile(self, client):
        resp = client.post(
            "/api/profiles", json={"name": "Before", "slug": "update-me"}
        )
        pid = resp.json()["id"]
        resp2 = client.patch(f"/api/profiles/{pid}", json={"name": "After"})
        assert resp2.status_code == 200
        assert resp2.json()["name"] == "After"

    def test_delete_profile(self, client):
        resp = client.post(
            "/api/profiles", json={"name": "Deletable", "slug": "deletable"}
        )
        pid = resp.json()["id"]
        resp2 = client.delete(f"/api/profiles/{pid}")
        assert resp2.status_code == 200
        assert resp2.json()["deleted"] == pid

        # Gone.
        resp3 = client.get(f"/api/profiles/{pid}")
        assert resp3.status_code == 404

    def test_delete_builtin_403(self, client):
        # Find a builtin profile.
        profiles = client.get("/api/profiles").json()
        builtin = next(p for p in profiles if p["is_builtin"])
        resp = client.delete(f"/api/profiles/{builtin['id']}")
        assert resp.status_code == 403

    def test_get_nonexistent_404(self, client):
        resp = client.get("/api/profiles/99999")
        assert resp.status_code == 404

    def test_seed_endpoint(self, client):
        resp = client.post("/api/profiles/seed")
        assert resp.status_code == 200
        # Idempotent: second call creates 0.
        resp2 = client.post("/api/profiles/seed")
        assert resp2.json()["created"] == 0

    def test_conversation_with_profile_id(self, client):
        # Get a profile id.
        profiles = client.get("/api/profiles").json()
        pid = profiles[0]["id"]

        resp = client.post(
            "/api/conversations",
            json={"title": "Profile chat", "profile_id": pid},
        )
        assert resp.status_code == 200
        conv = resp.json()
        assert conv["profile_id"] == pid

    def test_conversation_patch_profile_id(self, client):
        profiles = client.get("/api/profiles").json()
        pid = profiles[0]["id"]

        resp = client.post("/api/conversations", json={"title": "Patch test"})
        conv_id = resp.json()["id"]

        resp2 = client.patch(
            f"/api/conversations/{conv_id}", json={"profile_id": pid}
        )
        assert resp2.status_code == 200
        assert resp2.json()["profile_id"] == pid
