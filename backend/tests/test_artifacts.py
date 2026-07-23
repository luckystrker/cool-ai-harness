"""Tests for the artifacts API: upload, download, list, detail, delete (Фаза 1.5 §3)."""

from __future__ import annotations

import io
from pathlib import Path

import pytest
from fastapi.testclient import TestClient


@pytest.fixture(autouse=True)
def _artifacts_dir(tmp_path: Path, monkeypatch) -> Path:
    """Redirect artifact storage to a temp dir for test isolation."""
    from app.core import config as config_module

    art_dir = tmp_path / "artifacts"
    art_dir.mkdir()
    settings = config_module.get_settings()
    monkeypatch.setattr(settings, "artifacts_dir", art_dir)
    return art_dir


def _client() -> TestClient:
    from app.main import app

    return TestClient(app)


def _create_conversation(c: TestClient) -> int:
    resp = c.post("/api/conversations", json={"title": "art test"})
    assert resp.status_code == 200, resp.text
    return resp.json()["id"]


def _upload(c: TestClient, conv_id: int, content: bytes, filename: str, **params) -> dict:
    resp = c.post(
        f"/api/conversations/{conv_id}/artifacts",
        files={"file": (filename, io.BytesIO(content), "application/octet-stream")},
        params=params,
    )
    assert resp.status_code == 201, resp.text
    return resp.json()


# --- upload ---


def test_upload_and_get_artifact() -> None:
    with _client() as c:
        conv_id = _create_conversation(c)
        body = _upload(c, conv_id, b"hello world", "test.txt")
        art = body["artifact"]
        assert art["filename"] == "test.txt"
        assert art["size_bytes"] == 11
        assert art["conversation_id"] == conv_id
        assert art["kind"] == "file"
        assert art["sha256"] is not None
        assert body["message"] == "uploaded"


def test_upload_infers_kind_image() -> None:
    with _client() as c:
        conv_id = _create_conversation(c)
        body = _upload(c, conv_id, b"\x89PNG\r\n", "photo.png")
        assert body["artifact"]["kind"] == "image"


def test_upload_infers_kind_code() -> None:
    with _client() as c:
        conv_id = _create_conversation(c)
        body = _upload(c, conv_id, b"print('hi')", "script.py")
        assert body["artifact"]["kind"] == "code"


def test_upload_infers_kind_document() -> None:
    with _client() as c:
        conv_id = _create_conversation(c)
        body = _upload(c, conv_id, b"%PDF-1.4", "report.pdf")
        assert body["artifact"]["kind"] == "document"


def test_upload_explicit_kind_override() -> None:
    with _client() as c:
        conv_id = _create_conversation(c)
        body = _upload(c, conv_id, b"data", "out.bin", kind="report")
        assert body["artifact"]["kind"] == "report"


def test_upload_invalid_kind_400() -> None:
    with _client() as c:
        conv_id = _create_conversation(c)
        resp = c.post(
            f"/api/conversations/{conv_id}/artifacts",
            files={"file": ("f.txt", io.BytesIO(b"x"), "text/plain")},
            params={"kind": "nonexistent"},
        )
        assert resp.status_code == 400
        assert "Invalid kind" in resp.text


def test_upload_empty_file_400() -> None:
    with _client() as c:
        conv_id = _create_conversation(c)
        resp = c.post(
            f"/api/conversations/{conv_id}/artifacts",
            files={"file": ("empty.txt", io.BytesIO(b""), "text/plain")},
        )
        assert resp.status_code == 400
        assert "Empty file" in resp.text


def test_upload_extracts_text_for_text_files() -> None:
    with _client() as c:
        conv_id = _create_conversation(c)
        content = b"line1\nline2\nline3"
        body = _upload(c, conv_id, content, "notes.txt")
        art_id = body["artifact"]["id"]

        # Detail should include extracted_text.
        resp = c.get(f"/api/conversations/{conv_id}/artifacts/{art_id}")
        assert resp.status_code == 200
        assert resp.json()["extracted_text"] == "line1\nline2\nline3"


def test_upload_size_limit_413() -> None:
    from app.core import config as config_module

    settings = config_module.get_settings()
    original = settings.artifact_max_upload_bytes
    try:
        settings.artifact_max_upload_bytes = 10  # tiny limit
        with _client() as c:
            conv_id = _create_conversation(c)
            resp = c.post(
                f"/api/conversations/{conv_id}/artifacts",
                files={"file": ("big.bin", io.BytesIO(b"x" * 100), "application/octet-stream")},
            )
            assert resp.status_code == 413
    finally:
        settings.artifact_max_upload_bytes = original


# --- list ---


def test_list_artifacts() -> None:
    with _client() as c:
        conv_id = _create_conversation(c)
        _upload(c, conv_id, b"aaa", "a.txt")
        _upload(c, conv_id, b"bbb", "b.py")

        resp = c.get(f"/api/conversations/{conv_id}/artifacts")
        assert resp.status_code == 200
        arts = resp.json()
        assert len(arts) == 2
        # Newest first.
        assert arts[0]["filename"] == "b.py"
        assert arts[1]["filename"] == "a.txt"


def test_list_artifacts_filter_by_kind() -> None:
    with _client() as c:
        conv_id = _create_conversation(c)
        _upload(c, conv_id, b"code", "x.py")
        _upload(c, conv_id, b"img", "y.png")

        resp = c.get(f"/api/conversations/{conv_id}/artifacts", params={"kind": "code"})
        assert resp.status_code == 200
        arts = resp.json()
        assert len(arts) == 1
        assert arts[0]["filename"] == "x.py"


def test_list_artifacts_empty() -> None:
    with _client() as c:
        conv_id = _create_conversation(c)
        resp = c.get(f"/api/conversations/{conv_id}/artifacts")
        assert resp.status_code == 200
        assert resp.json() == []


# --- download ---


def test_download_artifact() -> None:
    with _client() as c:
        conv_id = _create_conversation(c)
        content = b"download me"
        body = _upload(c, conv_id, content, "dl.txt")
        art_id = body["artifact"]["id"]

        resp = c.get(f"/api/conversations/{conv_id}/artifacts/{art_id}/download")
        assert resp.status_code == 200
        assert resp.content == content
        assert "dl.txt" in resp.headers.get("content-disposition", "")


def test_download_missing_artifact_404() -> None:
    with _client() as c:
        conv_id = _create_conversation(c)
        resp = c.get(f"/api/conversations/{conv_id}/artifacts/99999/download")
        assert resp.status_code == 404


# --- detail ---


def test_artifact_detail_includes_versions() -> None:
    with _client() as c:
        conv_id = _create_conversation(c)
        body = _upload(c, conv_id, b"v1", "versioned.txt")
        art_id = body["artifact"]["id"]

        resp = c.get(f"/api/conversations/{conv_id}/artifacts/{art_id}")
        assert resp.status_code == 200
        detail = resp.json()
        assert detail["id"] == art_id
        assert detail["version"] == 1
        assert isinstance(detail["versions"], list)
        assert len(detail["versions"]) == 1


def test_artifact_detail_wrong_conversation_404() -> None:
    with _client() as c:
        conv_id = _create_conversation(c)
        other_conv_id = _create_conversation(c)
        body = _upload(c, conv_id, b"data", "f.txt")
        art_id = body["artifact"]["id"]

        # Access from wrong conversation.
        resp = c.get(f"/api/conversations/{other_conv_id}/artifacts/{art_id}")
        assert resp.status_code == 404


# --- delete ---


def test_delete_artifact() -> None:
    with _client() as c:
        conv_id = _create_conversation(c)
        body = _upload(c, conv_id, b"to delete", "del.txt")
        art_id = body["artifact"]["id"]

        resp = c.delete(f"/api/conversations/{conv_id}/artifacts/{art_id}")
        assert resp.status_code == 200
        assert resp.json()["deleted"] == art_id

        # Now it's gone from listing.
        resp = c.get(f"/api/conversations/{conv_id}/artifacts")
        assert all(a["id"] != art_id for a in resp.json())

        # Detail returns 404.
        resp = c.get(f"/api/conversations/{conv_id}/artifacts/{art_id}")
        assert resp.status_code == 404


def test_delete_missing_artifact_404() -> None:
    with _client() as c:
        conv_id = _create_conversation(c)
        resp = c.delete(f"/api/conversations/{conv_id}/artifacts/99999")
        assert resp.status_code == 404


# --- deduplication ---


def test_duplicate_content_shares_storage() -> None:
    """Uploading the same content twice creates two DB rows but one blob."""
    with _client() as c:
        conv_id = _create_conversation(c)
        body1 = _upload(c, conv_id, b"same content", "first.txt")
        body2 = _upload(c, conv_id, b"same content", "second.txt")

        # Same SHA-256.
        assert body1["artifact"]["sha256"] == body2["artifact"]["sha256"]
        # Different IDs.
        assert body1["artifact"]["id"] != body2["artifact"]["id"]
