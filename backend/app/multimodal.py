"""Multimodal message assembly and local OCR helpers (Phase 4)."""

from __future__ import annotations

import base64
from io import BytesIO
from typing import Any

from sqlmodel import Session

from app.artifacts import get_artifact, get_artifact_file
from app.core.config import get_settings
from app.models.artifact import ARTIFACT_KIND_DOCUMENT, ARTIFACT_KIND_IMAGE, Artifact

SUPPORTED_IMAGE_TYPES = frozenset({"image/png", "image/jpeg", "image/webp", "image/gif"})


def validate_artifact_ids(
    session: Session, *, conversation_id: int, artifact_ids: list[int]
) -> list[Artifact]:
    """Resolve attachments and enforce conversation ownership."""
    if len(artifact_ids) > 10:
        raise ValueError("At most 10 artifacts may be attached to one message")
    artifacts: list[Artifact] = []
    for artifact_id in dict.fromkeys(artifact_ids):
        artifact = get_artifact(session, artifact_id)
        if artifact is None or artifact.conversation_id != conversation_id:
            raise ValueError(f"Artifact {artifact_id} not found in this conversation")
        artifacts.append(artifact)
    return artifacts


def build_multimodal_content(
    session: Session,
    *,
    conversation_id: int,
    text: str | None,
    artifact_ids: list[int] | None,
) -> str | list[dict[str, Any]] | None:
    """Build canonical provider-neutral content from stored attachments."""
    if not artifact_ids:
        return text
    artifacts = validate_artifact_ids(
        session, conversation_id=conversation_id, artifact_ids=artifact_ids
    )
    parts: list[dict[str, Any]] = []
    if text:
        parts.append({"type": "text", "text": text})
    for artifact in artifacts:
        path = get_artifact_file(artifact)
        if path is None:
            continue
        if artifact.kind == ARTIFACT_KIND_IMAGE and artifact.media_type in SUPPORTED_IMAGE_TYPES:
            parts.append(
                {
                    "type": "image",
                    "media_type": artifact.media_type,
                    "data": base64.b64encode(path.read_bytes()).decode("ascii"),
                    "artifact_id": artifact.id,
                }
            )
        elif artifact.extracted_text:
            parts.append(
                {
                    "type": "text",
                    "text": f"\n[Attachment: {artifact.filename}]\n{artifact.extracted_text}",
                }
            )
        else:
            parts.append(
                {
                    "type": "text",
                    "text": f"\n[Attached file: {artifact.filename}; no text extracted]",
                }
            )
    return parts or text


def extract_text_local(artifact: Artifact) -> str:
    """Extract text from an image/PDF using local optional runtimes."""
    path = get_artifact_file(artifact)
    if path is None:
        raise ValueError("Artifact file is missing")
    settings = get_settings()
    max_chars = settings.artifact_max_extracted_chars
    if artifact.kind == ARTIFACT_KIND_IMAGE:
        try:
            import pytesseract
            from PIL import Image
        except ImportError as exc:
            raise RuntimeError("OCR dependencies are not installed") from exc
        with Image.open(BytesIO(path.read_bytes())) as image:
            if image.width * image.height > settings.artifact_max_image_pixels:
                raise ValueError("Image dimensions exceed the OCR pixel limit")
            return pytesseract.image_to_string(image).strip()[:max_chars]
    if artifact.kind == ARTIFACT_KIND_DOCUMENT and artifact.media_type == "application/pdf":
        try:
            from pypdf import PdfReader
        except ImportError as exc:
            raise RuntimeError("PDF extraction dependency is not installed") from exc
        reader = PdfReader(str(path))
        chunks: list[str] = []
        length = 0
        for page in reader.pages[: settings.artifact_max_document_pages]:
            chunk = (page.extract_text() or "").strip()
            chunks.append(chunk)
            length += len(chunk) + 2
            if length >= max_chars:
                break
        return "\n\n".join(chunks).strip()[:max_chars]
    if artifact.extracted_text is not None:
        return artifact.extracted_text
    raise ValueError("OCR supports images and PDF documents")
