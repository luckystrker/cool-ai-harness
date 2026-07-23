"""Artifact routes: upload, download, list, detail, delete (Фаза 1.5 §3)."""

from __future__ import annotations

from fastapi import APIRouter, Depends, HTTPException, UploadFile
from fastapi.responses import FileResponse
from sqlmodel import Session

from app.api.schemas import ArtifactDetail, ArtifactOut, ArtifactUploadResponse
from app.artifacts import (
    get_artifact,
    get_artifact_file,
    get_artifact_versions,
    list_artifacts,
    soft_delete_artifact,
    store_artifact,
)
from app.core.db import get_session
from app.models.artifact import ARTIFACT_KINDS

router = APIRouter()


def _art_to_out(a) -> ArtifactOut:
    return ArtifactOut(
        id=a.id,
        conversation_id=a.conversation_id,
        run_id=a.run_id,
        tool_call_id=a.tool_call_id,
        filename=a.filename,
        media_type=a.media_type,
        kind=a.kind,
        size_bytes=a.size_bytes,
        sha256=a.sha256,
        version=a.version,
        parent_id=a.parent_id,
        metadata_=a.metadata_,
        created_at=a.created_at,
        updated_at=a.updated_at,
    )


@router.post(
    "/conversations/{conv_id}/artifacts",
    response_model=ArtifactUploadResponse,
    status_code=201,
)
async def upload_artifact(
    conv_id: int,
    file: UploadFile,
    run_id: int | None = None,
    kind: str | None = None,
    session: Session = Depends(get_session),
) -> ArtifactUploadResponse:
    """Upload a file as an artifact attached to a conversation.

    The file is stored content-addressed (SHA-256) and deduplicated on disk.
    Optional ``kind`` overrides auto-detection; ``run_id`` links the artifact
    to the agent run that produced it.
    """
    if kind and kind not in ARTIFACT_KINDS:
        raise HTTPException(
            status_code=400,
            detail=f"Invalid kind '{kind}'. Must be one of: {sorted(ARTIFACT_KINDS)}",
        )

    content = await file.read()
    if not content:
        raise HTTPException(status_code=400, detail="Empty file")

    filename = file.filename or "unnamed"
    media_type = file.content_type

    try:
        artifact = store_artifact(
            session,
            conversation_id=conv_id,
            filename=filename,
            content=content,
            run_id=run_id,
            kind=kind,
            media_type=media_type,
        )
    except ValueError as exc:
        raise HTTPException(status_code=413, detail=str(exc)) from exc

    return ArtifactUploadResponse(artifact=_art_to_out(artifact))


@router.get(
    "/conversations/{conv_id}/artifacts",
    response_model=list[ArtifactOut],
)
def get_artifacts(
    conv_id: int,
    run_id: int | None = None,
    kind: str | None = None,
    limit: int = 100,
    session: Session = Depends(get_session),
) -> list[ArtifactOut]:
    """List artifacts for a conversation (newest first).

    Optional filters: ``run_id``, ``kind``. Capped at 500.
    """
    artifacts = list_artifacts(
        session, conversation_id=conv_id, run_id=run_id, kind=kind, limit=limit
    )
    return [_art_to_out(a) for a in artifacts]


@router.get(
    "/conversations/{conv_id}/artifacts/{artifact_id}",
    response_model=ArtifactDetail,
)
def get_artifact_detail(
    conv_id: int,
    artifact_id: int,
    session: Session = Depends(get_session),
) -> ArtifactDetail:
    """Get artifact metadata including extracted text and version chain."""
    art = get_artifact(session, artifact_id)
    if art is None or art.conversation_id != conv_id:
        raise HTTPException(status_code=404, detail="Artifact not found")
    versions = get_artifact_versions(session, artifact_id)
    return ArtifactDetail(
        id=art.id,
        conversation_id=art.conversation_id,
        run_id=art.run_id,
        tool_call_id=art.tool_call_id,
        filename=art.filename,
        media_type=art.media_type,
        kind=art.kind,
        size_bytes=art.size_bytes,
        sha256=art.sha256,
        version=art.version,
        parent_id=art.parent_id,
        metadata_=art.metadata_,
        created_at=art.created_at,
        updated_at=art.updated_at,
        extracted_text=art.extracted_text,
        versions=[_art_to_out(v) for v in versions],
    )


@router.get("/conversations/{conv_id}/artifacts/{artifact_id}/download")
def download_artifact(
    conv_id: int,
    artifact_id: int,
    session: Session = Depends(get_session),
) -> FileResponse:
    """Download the artifact's raw file content."""
    art = get_artifact(session, artifact_id)
    if art is None or art.conversation_id != conv_id:
        raise HTTPException(status_code=404, detail="Artifact not found")
    path = get_artifact_file(art)
    if path is None:
        raise HTTPException(status_code=404, detail="Artifact file missing from storage")
    return FileResponse(
        path=path,
        media_type=art.media_type,
        filename=art.filename,
    )


@router.delete("/conversations/{conv_id}/artifacts/{artifact_id}")
def delete_artifact(
    conv_id: int,
    artifact_id: int,
    session: Session = Depends(get_session),
) -> dict:
    """Soft-delete an artifact (keeps blob on disk for integrity)."""
    art = get_artifact(session, artifact_id)
    if art is None or art.conversation_id != conv_id:
        raise HTTPException(status_code=404, detail="Artifact not found")
    soft_delete_artifact(session, artifact_id)
    return {"deleted": artifact_id}
