"""Tests for RSS / News Aggregator (Фаза 3b §6).

Covers: subscription CRUD, feed fetching with deduplication, entry queries,
mark-read, agent-facing RSS tools, and the REST API.
"""

from __future__ import annotations

from unittest.mock import patch

import pytest
from fastapi.testclient import TestClient
from sqlmodel import Session

from app.core.db import engine

# --- Sample RSS XML for mocking httpx responses ---

SAMPLE_RSS = """<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0">
  <channel>
    <title>Test Feed</title>
    <link>https://example.com</link>
    <item>
      <title>First Post</title>
      <link>https://example.com/1</link>
      <guid>guid-1</guid>
      <author>alice</author>
      <description>Hello world</description>
      <pubDate>Sat, 02 Aug 2026 10:00:00 GMT</pubDate>
    </item>
    <item>
      <title>Second Post</title>
      <link>https://example.com/2</link>
      <guid>guid-2</guid>
      <author>bob</author>
      <description>Another entry</description>
      <pubDate>Sat, 02 Aug 2026 11:00:00 GMT</pubDate>
    </item>
  </channel>
</rss>
"""

SAMPLE_RSS_UPDATED = """<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0">
  <channel>
    <title>Test Feed</title>
    <link>https://example.com</link>
    <item>
      <title>Third Post</title>
      <link>https://example.com/3</link>
      <guid>guid-3</guid>
      <author>carol</author>
      <description>New entry</description>
      <pubDate>Sat, 02 Aug 2026 12:00:00 GMT</pubDate>
    </item>
    <item>
      <title>First Post</title>
      <link>https://example.com/1</link>
      <guid>guid-1</guid>
      <author>alice</author>
      <description>Hello world</description>
      <pubDate>Sat, 02 Aug 2026 10:00:00 GMT</pubDate>
    </item>
  </channel>
</rss>
"""


class MockResponse:
    """Mimics httpx.Response for feed downloads."""

    def __init__(self, content: bytes, status_code: int = 200):
        self.content = content
        self.status_code = status_code

    def raise_for_status(self):
        if self.status_code >= 400:
            import httpx

            raise httpx.HTTPStatusError(
                "error", request=None, response=self  # type: ignore[arg-type]
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


# --- Service tests --------------------------------------------------------


def _mock_httpx_get(content: str):
    """Return a patch context that mocks httpx.get."""
    return patch("app.rss.service.httpx.get", return_value=MockResponse(content.encode()))


class TestSubscriptionCRUD:
    def test_subscribe_creates_subscription(self, session, user_id):
        from app.rss.service import subscribe

        with _mock_httpx_get(SAMPLE_RSS):
            sub = subscribe(session, user_id=user_id, url="https://crud-create.com/feed.xml")
        assert sub.id is not None
        assert sub.url == "https://crud-create.com/feed.xml"
        assert sub.title == "Test Feed"
        assert sub.site_url == "https://example.com"
        assert sub.enabled is True

    def test_subscribe_duplicate_raises(self, session, user_id):
        from app.rss.service import subscribe

        with _mock_httpx_get(SAMPLE_RSS):
            subscribe(session, user_id=user_id, url="https://crud-dup.com/feed.xml")
        with pytest.raises(ValueError, match="Already subscribed"), _mock_httpx_get(SAMPLE_RSS):
            subscribe(session, user_id=user_id, url="https://crud-dup.com/feed.xml")

    def test_subscribe_with_category(self, session, user_id):
        from app.rss.service import subscribe

        with _mock_httpx_get(SAMPLE_RSS):
            sub = subscribe(
                session, user_id=user_id, url="https://crud-cat.com/feed.xml", category="tech"
            )
        assert sub.category == "tech"

    def test_unsubscribe(self, session, user_id):
        from app.rss.service import subscribe, unsubscribe

        with _mock_httpx_get(SAMPLE_RSS):
            sub = subscribe(session, user_id=user_id, url="https://crud-unsub.com/feed.xml")
        assert unsubscribe(session, sub.id) is True
        assert unsubscribe(session, sub.id) is False

    def test_list_subscriptions(self, session, user_id):
        from app.rss.service import list_subscriptions, subscribe

        with _mock_httpx_get(SAMPLE_RSS):
            subscribe(session, user_id=user_id, url="https://crud-list-a.com/feed")
            subscribe(session, user_id=user_id, url="https://crud-list-b.com/feed", category="news")
        subs = list_subscriptions(session, user_id=user_id)
        assert len(subs) >= 2
        news = list_subscriptions(session, user_id=user_id, category="news")
        assert all(s.category == "news" for s in news)


class TestFeedFetch:
    def test_fetch_stores_entries(self, session, user_id):
        from app.rss.service import fetch_feed, subscribe

        with _mock_httpx_get(SAMPLE_RSS):
            sub = subscribe(session, user_id=user_id, url="https://fetch-stores.com/feed.xml")
        with _mock_httpx_get(SAMPLE_RSS):
            count = fetch_feed(session, sub)
        assert count == 2
        session.refresh(sub)
        assert sub.entry_count == 2
        assert sub.last_error is None

    def test_fetch_deduplicates(self, session, user_id):
        from app.rss.service import fetch_feed, subscribe

        with _mock_httpx_get(SAMPLE_RSS):
            sub = subscribe(session, user_id=user_id, url="https://fetch-dedup.com/feed.xml")
        with _mock_httpx_get(SAMPLE_RSS):
            fetch_feed(session, sub)
        # Fetch same feed again — no new entries.
        with _mock_httpx_get(SAMPLE_RSS):
            count = fetch_feed(session, sub)
        assert count == 0

    def test_fetch_picks_up_new_entries(self, session, user_id):
        from app.rss.service import fetch_feed, subscribe

        with _mock_httpx_get(SAMPLE_RSS):
            sub = subscribe(session, user_id=user_id, url="https://fetch-new.com/feed.xml")
        with _mock_httpx_get(SAMPLE_RSS):
            fetch_feed(session, sub)
        # Feed now has a third entry + the first one (duplicate).
        with _mock_httpx_get(SAMPLE_RSS_UPDATED):
            count = fetch_feed(session, sub)
        assert count == 1  # only guid-3 is new

    def test_fetch_records_error(self, session, user_id):
        from app.rss.service import fetch_feed, subscribe

        with _mock_httpx_get(SAMPLE_RSS):
            sub = subscribe(session, user_id=user_id, url="https://fetch-error.com/feed.xml")
        with patch(
            "app.rss.service.httpx.get",
            return_value=MockResponse(b"", status_code=500),
        ):
            count = fetch_feed(session, sub)
        assert count == 0
        session.refresh(sub)
        assert sub.last_error is not None


class TestEntries:
    def test_list_entries(self, session, user_id):
        from app.rss.service import fetch_feed, list_entries, subscribe

        with _mock_httpx_get(SAMPLE_RSS):
            sub = subscribe(session, user_id=user_id, url="https://entries-list.com/feed.xml")
        with _mock_httpx_get(SAMPLE_RSS):
            fetch_feed(session, sub)
        entries = list_entries(session, subscription_id=sub.id)
        assert len(entries) == 2
        # Newest first (by published_at).
        assert entries[0].title == "Second Post"

    def test_mark_entry_read(self, session, user_id):
        from app.rss.service import fetch_feed, list_entries, mark_entry_read, subscribe

        with _mock_httpx_get(SAMPLE_RSS):
            sub = subscribe(session, user_id=user_id, url="https://entries-read.com/feed.xml")
        with _mock_httpx_get(SAMPLE_RSS):
            fetch_feed(session, sub)
        entries = list_entries(session, subscription_id=sub.id)
        result = mark_entry_read(session, entries[0].id)
        assert result is not None
        assert result.is_read is True

    def test_unread_filter(self, session, user_id):
        from app.rss.service import fetch_feed, list_entries, mark_entry_read, subscribe

        with _mock_httpx_get(SAMPLE_RSS):
            sub = subscribe(session, user_id=user_id, url="https://entries-unread.com/feed.xml")
        with _mock_httpx_get(SAMPLE_RSS):
            fetch_feed(session, sub)
        entries = list_entries(session, subscription_id=sub.id)
        mark_entry_read(session, entries[0].id)
        unread = list_entries(session, subscription_id=sub.id, unread_only=True)
        assert len(unread) == 1


# --- API tests ------------------------------------------------------------


class TestRssAPI:
    def test_subscribe_endpoint(self, client):
        with _mock_httpx_get(SAMPLE_RSS):
            resp = client.post(
                "/api/rss/subscriptions",
                json={"url": "https://api-test.com/feed.xml", "category": "test"},
            )
        assert resp.status_code == 201
        data = resp.json()
        assert data["url"] == "https://api-test.com/feed.xml"
        assert data["category"] == "test"

    def test_list_subscriptions_endpoint(self, client):
        with _mock_httpx_get(SAMPLE_RSS):
            client.post("/api/rss/subscriptions", json={"url": "https://list-test.com/feed"})
        resp = client.get("/api/rss/subscriptions")
        assert resp.status_code == 200
        assert isinstance(resp.json(), list)

    def test_delete_subscription_endpoint(self, client):
        with _mock_httpx_get(SAMPLE_RSS):
            resp = client.post("/api/rss/subscriptions", json={"url": "https://del-test.com/feed"})
        sub_id = resp.json()["id"]
        resp = client.delete(f"/api/rss/subscriptions/{sub_id}")
        assert resp.status_code == 204

    def test_fetch_endpoint(self, client):
        with _mock_httpx_get(SAMPLE_RSS):
            resp = client.post("/api/rss/subscriptions", json={"url": "https://fetch-test.com/feed"})
        sub_id = resp.json()["id"]
        with _mock_httpx_get(SAMPLE_RSS):
            resp = client.post(f"/api/rss/subscriptions/{sub_id}/fetch")
        assert resp.status_code == 200
        assert resp.json()["new_entries"] == 2

    def test_entries_endpoint(self, client):
        with _mock_httpx_get(SAMPLE_RSS):
            resp = client.post("/api/rss/subscriptions", json={"url": "https://entries-test.com/feed"})
        sub_id = resp.json()["id"]
        with _mock_httpx_get(SAMPLE_RSS):
            client.post(f"/api/rss/subscriptions/{sub_id}/fetch")
        resp = client.get(f"/api/rss/subscriptions/{sub_id}/entries")
        assert resp.status_code == 200
        assert len(resp.json()) == 2

    def test_all_entries_endpoint(self, client):
        resp = client.get("/api/rss/entries")
        assert resp.status_code == 200
        assert isinstance(resp.json(), list)

    def test_mark_read_endpoint(self, client):
        with _mock_httpx_get(SAMPLE_RSS):
            resp = client.post("/api/rss/subscriptions", json={"url": "https://read-test.com/feed"})
        sub_id = resp.json()["id"]
        with _mock_httpx_get(SAMPLE_RSS):
            client.post(f"/api/rss/subscriptions/{sub_id}/fetch")
        entries = client.get(f"/api/rss/subscriptions/{sub_id}/entries").json()
        entry_id = entries[0]["id"]
        resp = client.post(f"/api/rss/entries/{entry_id}/read", json={"is_read": True})
        assert resp.status_code == 200
        assert resp.json()["is_read"] is True
