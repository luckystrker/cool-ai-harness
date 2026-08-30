"""Vision analysis and OCR tools backed by stored conversation artifacts."""

from __future__ import annotations

import asyncio
import time

from pydantic import Field
from sqlmodel import Session

from app.artifacts import get_artifact
from app.core.config import get_settings
from app.core.db import engine
from app.multimodal import build_multimodal_content, extract_text_local
from app.providers import Message, get_provider_for_model
from app.security.capabilities import Capability
from app.tools.base import ToolArgs, ToolResult, register_tool
from app.tools.context import get_run_context


class ImageAnalyzeArgs(ToolArgs):
    artifact_id: int = Field(description="Image artifact id from the current conversation")
    prompt: str = Field(
        default="Describe and analyze this image in detail.",
        description="Question or analysis instruction for the vision model",
    )
    model: str | None = Field(default=None, description="Optional vision-capable model override")


class OcrExtractArgs(ToolArgs):
    artifact_id: int = Field(description="Image or PDF artifact id")


async def image_analyze(
    *,
    artifact_id: int,
    prompt: str = "Describe and analyze this image in detail.",
    model: str | None = None,
) -> ToolResult:
    ctx = get_run_context()
    if ctx.conversation_id is None:
        return ToolResult.err("image_analyze requires a conversation run context")
    with Session(engine) as session:
        artifact = get_artifact(session, artifact_id)
        if artifact is None or artifact.conversation_id != ctx.conversation_id:
            return ToolResult.err("Image artifact not found in this conversation")
        content = build_multimodal_content(
            session,
            conversation_id=ctx.conversation_id,
            text=prompt,
            artifact_ids=[artifact_id],
        )
    selected_model = model or ctx.model
    if not selected_model:
        from app.agent.service import resolve_default_model

        with Session(engine) as session:
            selected_model = resolve_default_model(session)
    if not selected_model:
        return ToolResult.err("No model configured for vision analysis")
    started_at = time.monotonic()
    try:
        provider = get_provider_for_model(selected_model)
        result = await provider.chat_completion(
            [Message(role="user", content=content)],
            model=selected_model,
            temperature=0.2,
            max_tokens=2_000,
        )
    except Exception as exc:
        return ToolResult.err(f"Vision analysis failed: {exc}")
    return ToolResult.ok(
        result.content or "Vision model returned an empty response",
        artifact_id=artifact_id,
        model=selected_model,
        llm_model=selected_model,
        llm_provider=getattr(provider, "name", ""),
        llm_usage=vars(result.usage) if result.usage is not None else None,
        llm_duration_ms=int((time.monotonic() - started_at) * 1000),
    )


async def ocr_extract(*, artifact_id: int) -> ToolResult:
    ctx = get_run_context()
    if ctx.conversation_id is None:
        return ToolResult.err("ocr_extract requires a conversation run context")
    with Session(engine) as session:
        artifact = get_artifact(session, artifact_id)
        if artifact is None or artifact.conversation_id != ctx.conversation_id:
            return ToolResult.err("Artifact not found in this conversation")
        try:
            text = await asyncio.wait_for(
                asyncio.to_thread(extract_text_local, artifact),
                timeout=get_settings().artifact_extract_timeout_s,
            )
        except Exception as exc:
            return ToolResult.err(f"OCR extraction failed: {exc}")
        artifact.extracted_text = text
        metadata = dict(artifact.metadata_ or {})
        metadata["ocr"] = {"engine": "tesseract" if artifact.kind == "image" else "pypdf"}
        artifact.metadata_ = metadata
        session.add(artifact)
        session.commit()
    return ToolResult.ok(text or "No text detected", artifact_id=artifact_id)


def register_multimodal_tools() -> None:
    register_tool(
        name="image_analyze",
        description=(
            "Analyze an uploaded image with the active vision-capable LLM. "
            "Use for screenshots, diagrams, charts, UI mockups, and photos."
        ),
        args_model=ImageAnalyzeArgs,
        func=image_analyze,
        capabilities=frozenset({Capability.READ, Capability.NETWORK}),
    )
    register_tool(
        name="ocr_extract",
        description="Extract text from an uploaded image or PDF and save it on the artifact.",
        args_model=OcrExtractArgs,
        func=ocr_extract,
        dangerous=True,
        capabilities=frozenset({Capability.READ, Capability.WRITE}),
    )
