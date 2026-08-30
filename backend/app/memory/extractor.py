"""Post-session memory extraction (Фаза 3a).

After a conversation run completes (or reaches a significant milestone), the
extractor analyzes the recent messages and produces candidate memories:
- User preferences discovered during the conversation.
- Project facts learned.
- Procedures that worked.
- Episodic summaries of what happened.

Extraction is LLM-driven: a structured prompt asks the model to identify
durable memories from the conversation transcript. Results are validated
and deduplicated by MemoryService before storage.
"""

from __future__ import annotations

import json
from typing import Any

from sqlmodel import Session

from app.core.config import get_settings
from app.core.logging import get_logger
from app.memory.models import MEMORY_SOURCE_AGENT_EXTRACTION, SCOPE_GLOBAL
from app.providers import LLMProvider, Message, message_text

log = get_logger(__name__)

# Minimum messages in a run to trigger extraction.
MIN_MESSAGES_FOR_EXTRACTION = 6
# Minimum tool calls to trigger extraction.
MIN_TOOL_CALLS_FOR_EXTRACTION = 3

EXTRACTION_SYSTEM_PROMPT = """\
You are a memory extraction system. Analyze the conversation below and extract \
durable memories that would be useful in future sessions.

Return a JSON object with this exact structure:
{
  "user_preferences": [
    {"content": "...", "importance": 0.0-1.0, "confidence": 0.0-1.0}
  ],
  "project_facts": [
    {"content": "...", "importance": 0.0-1.0, "confidence": 0.0-1.0}
  ],
  "procedures": [
    {"content": "...", "importance": 0.0-1.0, "confidence": 0.0-1.0}
  ],
  "episode_summary": {
    "title": "Brief title of what happened",
    "summary": "2-3 sentence summary of the interaction",
    "outcome": "success|failure|partial|unknown",
    "importance": 0.0-1.0
  }
}

Rules:
- Only extract information useful in FUTURE sessions.
- Do NOT store temporary debug details, one-time commands, or secrets.
- Do NOT store information already obvious from the project structure.
- Include confidence based on how explicitly the information was stated.
- user_preferences: explicit or strongly implied user preferences.
- project_facts: non-obvious facts about the project, stack, or architecture.
- procedures: reusable how-to knowledge (commands, workflows, patterns).
- episode_summary: always provide a brief summary of what happened.
- If nothing is worth remembering, return empty arrays and a minimal episode.
- Return ONLY valid JSON, no markdown fences or extra text.
"""


async def extract_memories_from_conversation(
    session: Session,
    *,
    provider: LLMProvider,
    model: str,
    user_id: int,
    conversation_id: int,
    agent_id: int | None = None,
    messages: list[Message] | None = None,
    run_id: int | None = None,
) -> dict[str, Any]:
    """Extract durable memories from a conversation's recent messages.

    Returns a summary dict of what was extracted (for logging/debugging).
    """
    settings = get_settings()
    if not settings.memory_extraction_enabled:
        return {"skipped": True, "reason": "extraction_disabled"}

    # Load messages if not provided.
    if messages is None:
        from app.agent.service import load_history

        messages = load_history(session, conversation_id)

    # Skip if too few messages.
    if len(messages) < MIN_MESSAGES_FOR_EXTRACTION:
        return {"skipped": True, "reason": "too_few_messages"}

    # Build the conversation transcript for the extraction prompt.
    transcript = _build_transcript(messages)
    if not transcript:
        return {"skipped": True, "reason": "empty_transcript"}

    # Use the configured summary model or the conversation model.
    extraction_model = settings.memory_summary_model or model

    # Call the LLM for extraction.
    try:
        result = await provider.chat_completion(
            [
                Message(role="system", content=EXTRACTION_SYSTEM_PROMPT),
                Message(role="user", content=f"Conversation transcript:\n\n{transcript}"),
            ],
            model=extraction_model,
            temperature=0.2,  # Low temperature for structured extraction.
            max_tokens=2000,
        )
    except Exception as exc:
        log.warning("memory.extraction_failed", error=str(exc))
        return {"skipped": True, "reason": "llm_error", "error": str(exc)}

    # Parse the extraction result.
    content = result.content or ""
    extracted = _parse_extraction_result(content)
    if extracted is None:
        log.warning("memory.extraction_parse_failed", content_preview=content[:200])
        return {"skipped": True, "reason": "parse_error"}

    # Store extracted memories.
    from app.memory.service import create_episode, remember

    # Conflict detection (C3, variant B — LLM): for each candidate fact, decide
    # whether it contradicts/updates an active memory; the resulting
    # supersedes_id makes confirming the new fact archive the old one.
    supersedes: dict[str, dict[str, int | None]] = {}
    if settings.memory_conflict_check_enabled:
        supersedes = await detect_conflicts_with_active(
            session,
            provider=provider,
            model=extraction_model,
            user_id=user_id,
            extracted=extracted,
        )

    stored_count = 0
    stored_memories: list = []

    # User preferences.
    for pref in extracted.get("user_preferences", []):
        memory = remember(
            session,
            user_id=user_id,
            content=pref["content"],
            memory_type="preference",
            scope=SCOPE_GLOBAL,
            importance=pref.get("importance", 0.7),
            confidence=pref.get("confidence", 0.7),
            source=MEMORY_SOURCE_AGENT_EXTRACTION,
            conversation_id=conversation_id,
            supersedes_id=supersedes.get("preference", {}).get(pref["content"]),
        )
        stored_memories.append(memory)
        stored_count += 1

    # Project facts.
    for fact in extracted.get("project_facts", []):
        memory = remember(
            session,
            user_id=user_id,
            content=fact["content"],
            memory_type="semantic",
            scope=SCOPE_GLOBAL,
            importance=fact.get("importance", 0.5),
            confidence=fact.get("confidence", 0.6),
            source=MEMORY_SOURCE_AGENT_EXTRACTION,
            conversation_id=conversation_id,
            supersedes_id=supersedes.get("fact", {}).get(fact["content"]),
        )
        stored_memories.append(memory)
        stored_count += 1

    # Procedures.
    for proc in extracted.get("procedures", []):
        memory = remember(
            session,
            user_id=user_id,
            content=proc["content"],
            memory_type="procedural",
            scope=SCOPE_GLOBAL,
            agent_id=agent_id,
            importance=proc.get("importance", 0.5),
            confidence=proc.get("confidence", 0.6),
            source=MEMORY_SOURCE_AGENT_EXTRACTION,
            conversation_id=conversation_id,
            supersedes_id=supersedes.get("procedure", {}).get(proc["content"]),
        )
        stored_memories.append(memory)
        stored_count += 1

    # Episode summary.
    episode_data = extracted.get("episode_summary")
    if episode_data and episode_data.get("title"):
        create_episode(
            session,
            user_id=user_id,
            agent_id=agent_id,
            conversation_id=conversation_id,
            run_id=run_id,
            title=episode_data["title"],
            summary=episode_data.get("summary", ""),
            outcome=episode_data.get("outcome", "unknown"),
            importance=episode_data.get("importance", 0.5),
        )

    # Entity extraction: pull named entities out of the transcript and link them
    # to the anchor memory (the most important one stored this run), so
    # entity-driven recall (C2) can find this memory later. Best-effort.
    entities_count = 0
    try:
        from app.memory.entities import extract_entities_from_text

        anchor = max(stored_memories, key=lambda m: m.importance, default=None)
        created_entities = await extract_entities_from_text(
            session,
            provider=provider,
            model=extraction_model,
            user_id=user_id,
            text=transcript,
            link_memory_id=anchor.id if anchor is not None else None,
        )
        entities_count = len(created_entities)
    except Exception as exc:  # extraction is best-effort
        log.warning("memory.entity_extraction_skipped", error=str(exc))

    log.info(
        "memory.extraction_complete",
        conversation_id=conversation_id,
        stored_count=stored_count,
        entities_count=entities_count,
        conflicts=len(supersedes),
    )
    return {"stored_count": stored_count, "extracted": extracted, "entities_count": entities_count}


def should_extract(message_count: int, tool_call_count: int) -> bool:
    """Determine if extraction should run based on conversation activity."""
    return (
        message_count >= MIN_MESSAGES_FOR_EXTRACTION
        or tool_call_count >= MIN_TOOL_CALLS_FOR_EXTRACTION
    )


def _build_transcript(messages: list[Message], max_chars: int = 8000) -> str:
    """Build a compact transcript from messages for the extraction prompt."""
    lines: list[str] = []
    total_chars = 0

    # Take the most recent messages (skip system prompt).
    relevant = [m for m in messages if m.role != "system"]
    # If too many, take the last N.
    if len(relevant) > 30:
        relevant = relevant[-30:]

    for msg in relevant:
        role = msg.role
        content = message_text(msg.content)
        # Truncate long tool outputs.
        if role == "tool" and len(content) > 300:
            content = content[:300] + "… (truncated)"
        elif len(content) > 500:
            content = content[:500] + "…"

        line = f"{role}: {content}"
        if total_chars + len(line) > max_chars:
            break
        lines.append(line)
        total_chars += len(line)

    return "\n".join(lines)


def _parse_extraction_result(content: str) -> dict[str, Any] | None:
    """Parse the LLM's extraction output as JSON."""
    # Strip markdown fences if present.
    content = content.strip()
    if content.startswith("```"):
        # Remove first and last lines (fences).
        lines = content.split("\n")
        if lines[0].startswith("```"):
            lines = lines[1:]
        if lines and lines[-1].strip() == "```":
            lines = lines[:-1]
        content = "\n".join(lines)

    try:
        return json.loads(content)
    except json.JSONDecodeError:
        # Try to find JSON within the content.
        start = content.find("{")
        end = content.rfind("}") + 1
        if start >= 0 and end > start:
            try:
                return json.loads(content[start:end])
            except json.JSONDecodeError:
                pass
    return None


CONFLICT_CHECK_SYSTEM_PROMPT = """\
You compare newly extracted facts against existing memories. For each new fact, \
decide whether it contradicts/updates an existing memory (same topic, different \
or corrected content) or merely complements it.

Existing memories are given as: id: "<content>".
New facts are indexed 0..N-1.

Return ONLY valid JSON:
{"results": [{"index": 0, "conflict_with": null | <existing id>, "kind": "contradicts|updates|complements|duplicate"}]}

Rules:
- "contradicts"/"updates": the new fact replaces or corrects the existing one.
- "complements": adds new information, no conflict.
- "duplicate": same meaning as an existing memory (should be merged, not kept).
- If unsure, prefer "complements" with conflict_with null.
- No markdown fences, no extra text.
"""


async def detect_conflicts_with_active(
    session: Session,
    *,
    provider: LLMProvider,
    model: str,
    user_id: int,
    extracted: dict[str, Any],
) -> dict[str, dict[str, int | None]]:
    """LLM conflict pass over extracted facts (C3, variant B).

    Returns {bucket: {content: superseded_memory_id | None}} — bucket is one of
    "preference" / "fact" / "procedure". Best-effort: on any failure returns
    empty mappings (the mechanical supersede path in ``remember`` still runs).
    """
    from app.memory.service import find_similar_active_memories

    # Collect candidates: (bucket, index, content, existing_memories).
    batches: list[dict] = []
    for bucket, items in (
        ("preference", extracted.get("user_preferences", [])),
        ("fact", extracted.get("project_facts", [])),
        ("procedure", extracted.get("procedures", [])),
    ):
        for item in items:
            content_text = item.get("content", "").strip() if isinstance(item, dict) else ""
            if not content_text:
                continue
            candidates = find_similar_active_memories(
                session,
                user_id=user_id,
                content=content_text,
                min_overlap=0.3,
                limit=3,
            )
            batches.append(
                {
                    "bucket": bucket,
                    "content": content_text,
                    "candidates": candidates,
                }
            )

    if not batches:
        return {}

    # Build the prompt: existing memories by id, new facts by index.
    seen_ids: set[int] = set()
    existing_lines: list[str] = []
    for b in batches:
        for mem, _ in b["candidates"]:
            if mem.id is not None and mem.id not in seen_ids:
                seen_ids.add(mem.id)
                existing_lines.append(f'{mem.id}: "{mem.content[:200]}"')
    new_facts_lines = [f'{i}: "{b["content"][:200]}"' for i, b in enumerate(batches)]

    prompt = (
        "Existing memories:\n"
        + ("\n".join(existing_lines) if existing_lines else "(none)")
        + "\n\nNew facts:\n"
        + "\n".join(new_facts_lines)
    )

    try:
        result = await provider.chat_completion(
            [
                Message(role="system", content=CONFLICT_CHECK_SYSTEM_PROMPT),
                Message(role="user", content=prompt),
            ],
            model=model,
            temperature=0.1,
            max_tokens=800,
        )
        raw = (result.content or "").strip()
        # Strip markdown fences if the model wrapped the JSON.
        if raw.startswith("```"):
            raw = raw.split("\n", 1)[-1]
            if raw.endswith("```"):
                raw = raw.rsplit("```", 1)[0]
            raw = raw.strip()
        parsed = json.loads(raw)
    except Exception as exc:
        log.warning("memory.conflict_check_failed", error=str(exc))
        return {}

    results = parsed.get("results") if isinstance(parsed, dict) else None
    if not isinstance(results, list):
        return {}

    supersedes: dict[str, dict[str, int | None]] = {}
    for row in results:
        if not isinstance(row, dict):
            continue
        index = row.get("index")
        if not isinstance(index, int) or not (0 <= index < len(batches)):
            continue
        kind = row.get("kind")
        conflict_id = row.get("conflict_with")
        if kind not in ("contradicts", "updates"):
            continue
        if not isinstance(conflict_id, int) or conflict_id not in seen_ids:
            continue
        batch = batches[index]
        bucket_map = supersedes.setdefault(batch["bucket"], {})
        bucket_map[batch["content"]] = conflict_id

    return supersedes
