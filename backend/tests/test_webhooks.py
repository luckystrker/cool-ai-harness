"""Tests for Webhook Router (Фаза 3b §7).

Covers: endpoint CRUD, HMAC signature verification, event processing,
event type filtering, replay, and the REST API (management + public inbound).
"""

from __future__ import annotations

import hashlib
import hmac
import json

import pytest
from fastapi.testclient import TestClient
from sqlmodel import Session

from app.core.db import engine
from app.models.webhook import (
    EVENT_COMPLETED,
    EVENT_REJECTED,
)
from app.webhooks.service import (
    compute_signature,
    create_endpoint,
    delete_endpoint,
    get_endpoint,
    get_endpoint_by_hook_id,
    list_endpoints,
    list_events,
    process_event,
    replay_event,
    update_endpoint,
    verify_signature,
)


@pytest.fixture()
def session():
    with Session(engine) as s:
        yield s


@pytest.fixture()
def user_id(session):
    from app.agent.service import get_or_create_default_user

    user = get_or_create_default_user(session)
    return user.id


@pytest.fixture()
def client():
    from app.main import app

    return TestClient(app)


@pytest.fixture()
def endpoint(session, user_id):
    """A ready-made webhook endpoint."""
    return create_endpoint(
        session,
        user_id=user_id,
        name="Test Hook",
        source_type="github",
    )


# --- Signature verification -----------------------------------------------


class TestSignature:
    def test_valid_signature(self):
        secret = "my-secret"
        body = b'{"event": "push"}'
        sig = compute_signature(secret, body)
        assert verify_signature(secret, body, sig) is True

    def test_github_prefix(self):
        secret = "my-secret"
        body = b'{"event": "push"}'
        sig = "sha256=" + compute_signature(secret, body)
        assert verify_signature(secret, body, sig) is True

    def test_invalid_signature(self):
        assert verify_signature("secret", b"body", "deadbeef") is False

    def test_missing_signature(self):
        assert verify_signature("secret", b"body", None) is False

    def test_empty_signature(self):
        assert verify_signature("secret", b"body", "") is False


# --- Endpoint CRUD --------------------------------------------------------


class TestEndpointCRUD:
    def test_create_endpoint(self, session, user_id):
        ep = create_endpoint(session, user_id=user_id, name="My Hook", source_type="custom")
        assert ep.id is not None
        assert ep.hook_id  # UUID hex
        assert ep.secret  # HMAC secret
        assert ep.source_type == "custom"
        assert ep.enabled is True

    def test_create_invalid_source_type(self, session, user_id):
        with pytest.raises(ValueError, match="Unknown source_type"):
            create_endpoint(session, user_id=user_id, name="Bad", source_type="invalid")

    def test_get_endpoint(self, session, endpoint):
        fetched = get_endpoint(session, endpoint.id)
        assert fetched is not None
        assert fetched.name == "Test Hook"

    def test_get_by_hook_id(self, session, endpoint):
        fetched = get_endpoint_by_hook_id(session, endpoint.hook_id)
        assert fetched is not None
        assert fetched.id == endpoint.id

    def test_list_endpoints(self, session, user_id):
        create_endpoint(session, user_id=user_id, name="Hook A")
        create_endpoint(session, user_id=user_id, name="Hook B")
        eps = list_endpoints(session, user_id=user_id)
        assert len(eps) >= 2

    def test_update_endpoint(self, session, endpoint):
        updated = update_endpoint(session, endpoint.id, name="Renamed", enabled=False)
        assert updated is not None
        assert updated.name == "Renamed"
        assert updated.enabled is False

    def test_delete_endpoint(self, session, endpoint):
        assert delete_endpoint(session, endpoint.id) is True
        assert get_endpoint(session, endpoint.id) is None
        assert delete_endpoint(session, endpoint.id) is False


# --- Event processing -----------------------------------------------------


class TestEventProcessing:
    def test_valid_event_accepted(self, session, endpoint):
        event = process_event(
            session,
            endpoint,
            payload={"action": "opened", "number": 1},
            signature_valid=True,
            event_type="pull_request",
        )
        assert event.id is not None
        assert event.signature_valid is True
        assert event.status == EVENT_COMPLETED
        assert event.event_type == "pull_request"

    def test_invalid_signature_rejected(self, session, endpoint):
        event = process_event(
            session,
            endpoint,
            payload={"foo": "bar"},
            signature_valid=False,
        )
        assert event.status == EVENT_REJECTED
        assert "signature" in (event.error or "").lower()

    def test_event_filter_rejects_unlisted(self, session, user_id):
        ep = create_endpoint(
            session,
            user_id=user_id,
            name="Filtered",
            event_filter=["push"],
        )
        event = process_event(
            session,
            ep,
            payload={"action": "opened"},
            signature_valid=True,
            event_type="pull_request",
        )
        assert event.status == EVENT_REJECTED
        assert "filter" in (event.error or "").lower()

    def test_event_filter_accepts_listed(self, session, user_id):
        ep = create_endpoint(
            session,
            user_id=user_id,
            name="Filtered OK",
            event_filter=["push"],
        )
        event = process_event(
            session,
            ep,
            payload={"ref": "main"},
            signature_valid=True,
            event_type="push",
        )
        assert event.status == EVENT_COMPLETED

    def test_github_event_type_extraction(self, session, endpoint):
        event = process_event(
            session,
            endpoint,
            payload={"action": "opened"},
            signature_valid=True,
            headers={"x-github-event": "issues"},
        )
        assert event.event_type == "issues"


class TestEventQueries:
    def test_list_events(self, session, endpoint):
        process_event(session, endpoint, payload={"a": 1}, signature_valid=True)
        process_event(session, endpoint, payload={"b": 2}, signature_valid=True)
        events = list_events(session, endpoint_id=endpoint.id)
        assert len(events) == 2

    def test_replay_event(self, session, endpoint):
        original = process_event(
            session, endpoint, payload={"x": 1}, signature_valid=True, event_type="test"
        )
        replayed = replay_event(session, original.id)
        assert replayed is not None
        assert replayed.id != original.id
        assert replayed.payload == original.payload
        assert replayed.status == EVENT_COMPLETED


# --- API tests ------------------------------------------------------------


class TestWebhookManagementAPI:
    def test_create_endpoint(self, client):
        resp = client.post(
            "/api/webhooks",
            json={"name": "API Hook", "source_type": "github"},
        )
        assert resp.status_code == 201
        data = resp.json()
        assert data["name"] == "API Hook"
        assert data["hook_id"]
        assert data["secret"]
        assert "url_path" in data

    def test_list_endpoints(self, client):
        client.post("/api/webhooks", json={"name": "List Hook"})
        resp = client.get("/api/webhooks")
        assert resp.status_code == 200
        assert isinstance(resp.json(), list)

    def test_get_endpoint(self, client):
        resp = client.post("/api/webhooks", json={"name": "Get Hook"})
        ep_id = resp.json()["id"]
        resp = client.get(f"/api/webhooks/{ep_id}")
        assert resp.status_code == 200
        assert resp.json()["name"] == "Get Hook"

    def test_update_endpoint(self, client):
        resp = client.post("/api/webhooks", json={"name": "Update Hook"})
        ep_id = resp.json()["id"]
        resp = client.put(f"/api/webhooks/{ep_id}", json={"name": "Updated", "enabled": False})
        assert resp.status_code == 200
        assert resp.json()["name"] == "Updated"
        assert resp.json()["enabled"] is False

    def test_delete_endpoint(self, client):
        resp = client.post("/api/webhooks", json={"name": "Delete Hook"})
        ep_id = resp.json()["id"]
        resp = client.delete(f"/api/webhooks/{ep_id}")
        assert resp.status_code == 204

    def test_events_endpoint(self, client):
        resp = client.post("/api/webhooks", json={"name": "Events Hook"})
        ep_id = resp.json()["id"]
        resp = client.get(f"/api/webhooks/{ep_id}/events")
        assert resp.status_code == 200
        assert isinstance(resp.json(), list)


class TestWebhookInboundAPI:
    def test_receive_valid_event(self, client):
        # Create endpoint first.
        resp = client.post("/api/webhooks", json={"name": "Inbound Hook", "source_type": "custom"})
        data = resp.json()
        hook_id = data["hook_id"]
        secret = data["secret"]

        # POST with valid signature.
        payload = json.dumps({"event": "test", "data": 42}).encode()
        sig = hmac.new(secret.encode(), payload, hashlib.sha256).hexdigest()
        resp = client.post(
            f"/api/webhooks/inbound/{hook_id}",
            content=payload,
            headers={"Content-Type": "application/json", "X-Signature": sig},
        )
        assert resp.status_code == 200
        result = resp.json()
        assert result["status"] == "completed"
        assert result["event_type"] == "test"

    def test_receive_invalid_signature(self, client):
        resp = client.post("/api/webhooks", json={"name": "Bad Sig Hook"})
        hook_id = resp.json()["hook_id"]

        payload = json.dumps({"event": "hack"}).encode()
        resp = client.post(
            f"/api/webhooks/inbound/{hook_id}",
            content=payload,
            headers={"Content-Type": "application/json", "X-Signature": "invalid"},
        )
        assert resp.status_code == 200
        assert resp.json()["status"] == "rejected"

    def test_unknown_hook_id(self, client):
        resp = client.post(
            "/api/webhooks/inbound/nonexistent",
            content=b"{}",
            headers={"Content-Type": "application/json"},
        )
        assert resp.status_code == 404

    def test_disabled_endpoint(self, client):
        resp = client.post("/api/webhooks", json={"name": "Disabled Hook", "enabled": False})
        hook_id = resp.json()["hook_id"]
        resp = client.post(
            f"/api/webhooks/inbound/{hook_id}",
            content=b"{}",
            headers={"Content-Type": "application/json"},
        )
        assert resp.status_code == 410

    def test_replay_endpoint(self, client):
        # Create + receive an event.
        resp = client.post("/api/webhooks", json={"name": "Replay Hook"})
        data = resp.json()
        ep_id = data["id"]
        hook_id = data["hook_id"]
        secret = data["secret"]

        payload = json.dumps({"event": "replay_test"}).encode()
        sig = hmac.new(secret.encode(), payload, hashlib.sha256).hexdigest()
        client.post(
            f"/api/webhooks/inbound/{hook_id}",
            content=payload,
            headers={"Content-Type": "application/json", "X-Signature": sig},
        )

        # Get events, replay the first one.
        events = client.get(f"/api/webhooks/{ep_id}/events").json()
        assert len(events) >= 1
        event_id = events[0]["id"]
        resp = client.post(f"/api/webhooks/{ep_id}/events/{event_id}/replay")
        assert resp.status_code == 200
        assert resp.json()["status"] == "completed"
