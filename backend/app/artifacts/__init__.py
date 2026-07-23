"""Artifact storage and management (Фаза 1.5 §3).

Provides content-addressed blob storage on the local filesystem plus DB-backed
metadata, versioning, and provenance tracking. The service layer keeps file I/O
out of route handlers and the agent loop.
"""

from __future__ import annotations

import hashlib
import mimetypes
from pathlib import Path

from sqlmodel import Session, select

from app.core.config import get_settings
from app.core.logging import get_logger
from app.models.artifact import (
    ARTIFACT_KIND_AUDIO,
    ARTIFACT_KIND_CODE,
    ARTIFACT_KIND_DOCUMENT,
    ARTIFACT_KIND_FILE,
    ARTIFACT_KIND_IMAGE,
    ARTIFACT_KIND_REPORT,
    Artifact,
)

log = get_logger(__name__)

# Extension → kind mapping for common file types.
_EXT_KIND_MAP: dict[str, str] = {
    # Images
    ".png": ARTIFACT_KIND_IMAGE,
    ".jpg": ARTIFACT_KIND_IMAGE,
    ".jpeg": ARTIFACT_KIND_IMAGE,
    ".gif": ARTIFACT_KIND_IMAGE,
    ".webp": ARTIFACT_KIND_IMAGE,
    ".svg": ARTIFACT_KIND_IMAGE,
    ".bmp": ARTIFACT_KIND_IMAGE,
    # Documents
    ".pdf": ARTIFACT_KIND_DOCUMENT,
    ".doc": ARTIFACT_KIND_DOCUMENT,
    ".docx": ARTIFACT_KIND_DOCUMENT,
    ".odt": ARTIFACT_KIND_DOCUMENT,
    ".rtf": ARTIFACT_KIND_DOCUMENT,
    # Code
    ".py": ARTIFACT_KIND_CODE,
    ".js": ARTIFACT_KIND_CODE,
    ".ts": ARTIFACT_KIND_CODE,
    ".tsx": ARTIFACT_KIND_CODE,
    ".jsx": ARTIFACT_KIND_CODE,
    ".rs": ARTIFACT_KIND_CODE,
    ".go": ARTIFACT_KIND_CODE,
    ".java": ARTIFACT_KIND_CODE,
    ".c": ARTIFACT_KIND_CODE,
    ".cpp": ARTIFACT_KIND_CODE,
    ".h": ARTIFACT_KIND_CODE,
    ".rb": ARTIFACT_KIND_CODE,
    ".sh": ARTIFACT_KIND_CODE,
    ".sql": ARTIFACT_KIND_CODE,
    # Audio
    ".mp3": ARTIFACT_KIND_AUDIO,
    ".wav": ARTIFACT_KIND_AUDIO,
    ".ogg": ARTIFACT_KIND_AUDIO,
    ".flac": ARTIFACT_KIND_AUDIO,
    ".m4a": ARTIFACT_KIND_AUDIO,
    # Reports (markdown/text treated as report when produced by agent)
    ".md": ARTIFACT_KIND_REPORT,
}

# Extensions that are plain-text and can be read directly for extracted_text.
_TEXT_EXTENSIONS: set[str] = {
    ".txt", ".py", ".js", ".ts", ".tsx", ".jsx", ".rs", ".go", ".java",
    ".c", ".cpp", ".h", ".rb", ".sh", ".sql", ".md", ".json", ".yaml",
    ".yml", ".toml", ".ini", ".cfg", ".csv", ".xml", ".html", ".css",
    ".log", ".env", ".gitignore",
}


def infer_kind(filename: str, media_type: str | None = None) -> str:
    """Infer artifact kind from filename extension and/or MIME type."""
    ext = Path(filename).suffix.lower()
    if ext in _EXT_KIND_MAP:
        return _EXT_KIND_MAP[ext]
    if media_type:
        if media_type.startswith("image/"):
            return ARTIFACT_KIND_IMAGE
        if media_type.startswith("audio/"):
            return ARTIFACT_KIND_AUDIO
        if media_type in ("application/pdf",):
            return ARTIFACT_KIND_DOCUMENT
    return ARTIFACT_KIND_FILE


def _content_path(sha256_hex: str) -> str:
    """Compute the relative storage path from a SHA-256 digest (2-char fanout)."""
    return f"{sha256_hex[:2]}/{sha256_hex}"


def _is_text_file(filename: str) -> bool:
    ext = Path(filename).suffix.lower()
    return ext in _TEXT_EXTENSIONS


def store_artifact(
    session: Session,
    *,
    conversation_id: int,
    filename: str,
    content: bytes,
    run_id: int | None = None,
    tool_call_id: str | None = None,
    kind: str | None = None,
    media_type: str | None = None,
    metadata: dict | None = None,
    parent_id: int | None = None,
) -> Artifact:
    """Persist an artifact blob to disk and create its DB record.

    Content-addressed: if the same SHA-256 already exists on disk, the file is
    not written again (deduplication). The DB row is always created.
    """
    settings = get_settings()

    # Enforce upload size limit.
    max_bytes = settings.artifact_max_upload_bytes
    if max_bytes and len(content) > max_bytes:
        raise ValueError(
            f"File exceeds max upload size ({len(content)} > {max_bytes} bytes)"
        )

    # Compute hash and storage path.
    sha256_hex = hashlib.sha256(content).hexdigest()
    rel_path = _content_path(sha256_hex)
    abs_path = settings.artifacts_dir / rel_path

    # Write blob if not already present (dedup).
    if not abs_path.exists():
        abs_path.parent.mkdir(parents=True, exist_ok=True)
        abs_path.write_bytes(content)
        log.debug("artifact.stored", sha256=sha256_hex, size=len(content), path=str(rel_path))

    # Infer MIME type if not provided.
    if not media_type:
        media_type = mimetypes.guess_type(filename)[0] or "application/octet-stream"

    # Infer kind if not provided.
    if not kind:
        kind = infer_kind(filename, media_type)

    # Extract text for text-based files.
    extracted_text: str | None = None
    if _is_text_file(filename):
        try:
            text = content.decode("utf-8", errors="replace")
            max_chars = settings.artifact_max_extracted_chars
            if len(text) > max_chars:
                text = text[:max_chars]
            extracted_text = text
        except Exception:
            pass

    # Determine version.
    version = 1
    if parent_id is not None:
        parent = session.get(Artifact, parent_id)
        if parent:
            version = parent.version + 1

    artifact = Artifact(
        conversation_id=conversation_id,
        run_id=run_id,
        tool_call_id=tool_call_id,
        filename=filename,
        media_type=media_type,
        kind=kind,
        size_bytes=len(content),
        sha256=sha256_hex,
        storage_path=rel_path,
        version=version,
        parent_id=parent_id,
        metadata_=metadata,
        extracted_text=extracted_text,
    )
    session.add(artifact)
    session.commit()
    session.refresh(artifact)
    log.info(
        "artifact.created",
        id=artifact.id,
        filename=filename,
        kind=kind,
        size=len(content),
        conversation_id=conversation_id,
    )
    return artifact


def get_artifact(session: Session, artifact_id: int) -> Artifact | None:
    """Fetch an artifact by ID (returns None if not found or soft-deleted)."""
    art = session.get(Artifact, artifact_id)
    if art is None or art.is_deleted:
        return None
    return art


def get_artifact_file(artifact: Artifact) -> Path | None:
    """Return the absolute filesystem path for an artifact's blob, or None."""
    settings = get_settings()
    path = settings.artifacts_dir / artifact.storage_path
    return path if path.exists() else None


def list_artifacts(
    session: Session,
    *,
    conversation_id: int,
    run_id: int | None = None,
    kind: str | None = None,
    limit: int = 100,
) -> list[Artifact]:
    """List artifacts for a conversation, newest first."""
    stmt = (
        select(Artifact)
        .where(Artifact.conversation_id == conversation_id, Artifact.is_deleted == False)  # noqa: E712
        .order_by(Artifact.id.desc())
        .limit(min(limit, 500))
    )
    if run_id is not None:
        stmt = stmt.where(Artifact.run_id == run_id)
    if kind is not None:
        stmt = stmt.where(Artifact.kind == kind)
    return list(session.exec(stmt).all())


def soft_delete_artifact(session: Session, artifact_id: int) -> bool:
    """Soft-delete an artifact (marks is_deleted=True, keeps blob on disk)."""
    art = session.get(Artifact, artifact_id)
    if art is None or art.is_deleted:
        return False
    art.is_deleted = True
    session.add(art)
    session.commit()
    log.info("artifact.deleted", id=artifact_id, filename=art.filename)
    return True


def get_artifact_versions(session: Session, artifact_id: int) -> list[Artifact]:
    """Return the full version chain for an artifact (oldest first)."""
    art = session.get(Artifact, artifact_id)
    if art is None:
        return []
    # Walk up to root.
    chain: list[Artifact] = [art]
    current = art
    while current.parent_id is not None:
        parent = session.get(Artifact, current.parent_id)
        if parent is None:
            break
        chain.append(parent)
        current = parent
    chain.reverse()
    return chain
