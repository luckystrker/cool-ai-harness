"""Artifact model (Фаза 1.5 §3 — artifacts & attachments).

An ``Artifact`` is a unified record for any file produced or consumed by the
agent: uploaded attachments (PDF, images, text, audio), tool-call outputs
(generated code, research reports), and any other binary result worth
persisting. Each artifact is stored on disk under a content-addressed path and
referenced by its DB row for metadata, versioning, and provenance.
"""

from __future__ import annotations

from typing import Any

from sqlalchemy import Column, Text
from sqlalchemy.types import JSON
from sqlmodel import Field

from app.models.base import TimestampMixin

# Artifact kind constants — coarse classification for UI rendering and routing.
ARTIFACT_KIND_FILE = "file"
ARTIFACT_KIND_IMAGE = "image"
ARTIFACT_KIND_DOCUMENT = "document"
ARTIFACT_KIND_CODE = "code"
ARTIFACT_KIND_REPORT = "report"
ARTIFACT_KIND_AUDIO = "audio"
ARTIFACT_KIND_TOOL_RESULT = "tool_result"

ARTIFACT_KINDS = frozenset(
    {
        ARTIFACT_KIND_FILE,
        ARTIFACT_KIND_IMAGE,
        ARTIFACT_KIND_DOCUMENT,
        ARTIFACT_KIND_CODE,
        ARTIFACT_KIND_REPORT,
        ARTIFACT_KIND_AUDIO,
        ARTIFACT_KIND_TOOL_RESULT,
    }
)


class Artifact(TimestampMixin, table=True):
    """A stored file or blob associated with a conversation and optionally a run."""

    __tablename__ = "artifacts"

    id: int | None = Field(default=None, primary_key=True)
    conversation_id: int = Field(foreign_key="conversations.id", index=True)
    # The run that produced this artifact (None for user uploads).
    run_id: int | None = Field(default=None, foreign_key="agent_runs.id", index=True)
    # The tool call that generated this artifact (if applicable).
    tool_call_id: str | None = None
    # Human-readable filename (original upload name or generated name).
    filename: str
    # MIME type (e.g. "application/pdf", "image/png", "text/plain").
    media_type: str = "application/octet-stream"
    # Coarse kind for UI routing (file|image|document|code|report|audio|tool_result).
    kind: str = Field(default=ARTIFACT_KIND_FILE, index=True)
    # Size in bytes of the stored blob.
    size_bytes: int = 0
    # SHA-256 hex digest of the content (deduplication & integrity).
    sha256: str | None = Field(default=None, index=True)
    # Relative storage path under artifacts_dir (e.g. "ab/cdef1234...").
    storage_path: str
    # Version counter: increments when the same logical artifact is overwritten.
    version: int = 1
    # Parent artifact id (for version chains: this is a newer version of parent).
    parent_id: int | None = Field(default=None, foreign_key="artifacts.id")
    # Free-form metadata: page count, dimensions, transcription text, etc.
    metadata_: dict[str, Any] | None = Field(default=None, sa_column=Column("metadata_", JSON))
    # Extracted text content (for text-based artifacts, PDF extraction, etc.)
    # Stored separately so it can be indexed / fed to the LLM context window.
    extracted_text: str | None = Field(default=None, sa_column=Column(Text))
    # Whether this artifact has been soft-deleted.
    is_deleted: bool = Field(default=False, index=True)
