"""Context builder — assembles memory into a system prompt block.

Produces a structured text block injected into the agent's system prompt,
containing user preferences, relevant long-term memories, conversation
summary, and working memory state.

Assembly order (by priority):
1. User preferences (always included)
2. Relevant long-term memories (FTS5 recall based on user input)
3. Conversation summary (compressed older messages)
4. Working memory state (scratchpad: goal, hypotheses, entities)
"""

from __future__ import annotations

from sqlmodel import Session

from app.core.config import get_settings
from app.core.logging import get_logger
from app.memory.models import MEMORY_TYPE_PREFERENCE

log = get_logger(__name__)

# Max characters for the entire memory context block.
MAX_MEMORY_CONTEXT_CHARS = 4000
# Max memories to include in the context block.
MAX_CONTEXT_MEMORIES = 8
# Max characters per memory content in the context.
MAX_MEMORY_CONTENT_CHARS = 200


def build_memory_context(
    session: Session,
    *,
    user_id: int,
    agent_id: int | None = None,
    conversation_id: int | None = None,
    query: str | None = None,
) -> str | None:
    """Build the memory context block for injection into the system prompt.

    Returns None if memory is disabled or no relevant memories exist.
    """
    settings = get_settings()
    if not settings.memory_enabled:
        return None

    sections: list[str] = []

    # 1. User preferences (always included).
    prefs = _get_preferences_block(session, user_id=user_id)
    if prefs:
        sections.append(prefs)

    # 2. Relevant long-term memories (based on user's current query).
    if query:
        memories_block = _get_memories_block(
            session,
            user_id=user_id,
            agent_id=agent_id,
            conversation_id=conversation_id,
            query=query,
        )
        if memories_block:
            sections.append(memories_block)

    # 3. Conversation summary (compressed older messages).
    if conversation_id is not None:
        summary_block = _get_summary_block(session, conversation_id=conversation_id)
        if summary_block:
            sections.append(summary_block)

    # 4. Working memory state (scratchpad).
    if conversation_id is not None:
        wm_block = _get_working_memory_block(session, conversation_id=conversation_id)
        if wm_block:
            sections.append(wm_block)

    if not sections:
        return None

    context = "\n\n".join(sections)

    # Enforce total size limit.
    if len(context) > MAX_MEMORY_CONTEXT_CHARS:
        context = context[:MAX_MEMORY_CONTEXT_CHARS] + "\n… (memory context truncated)"

    return f"[MEMORY CONTEXT]\n{context}"


def _get_preferences_block(session: Session, *, user_id: int) -> str | None:
    """Build the user preferences section."""
    from app.memory.service import get_preferences

    prefs = get_preferences(session, user_id=user_id)
    if not prefs:
        return None

    lines = ["[USER PREFERENCES]"]
    for pref in prefs[:10]:  # Cap at 10 preferences.
        content = pref.content
        if len(content) > MAX_MEMORY_CONTENT_CHARS:
            content = content[:MAX_MEMORY_CONTENT_CHARS] + "…"
        lines.append(f"- {content}")

    return "\n".join(lines)


def _get_memories_block(
    session: Session,
    *,
    user_id: int,
    agent_id: int | None,
    conversation_id: int | None,
    query: str,
) -> str | None:
    """Build the relevant long-term memories section."""
    from app.memory.retrieval import retrieve_memories

    memories = retrieve_memories(
        session,
        user_id=user_id,
        query=query,
        agent_id=agent_id,
        conversation_id=conversation_id,
        limit=MAX_CONTEXT_MEMORIES,
    )

    # Exclude preferences (already in their own section).
    memories = [m for m in memories if m.memory_type != MEMORY_TYPE_PREFERENCE]
    if not memories:
        return None

    lines = ["[RELEVANT MEMORIES]"]
    for mem in memories:
        content = mem.content
        if len(content) > MAX_MEMORY_CONTENT_CHARS:
            content = content[:MAX_MEMORY_CONTENT_CHARS] + "…"
        # Add type tag for context.
        type_tag = f"({mem.memory_type})" if mem.memory_type != "semantic" else ""
        lines.append(f"- {type_tag} {content}".strip())

    return "\n".join(lines)


def _get_summary_block(session: Session, *, conversation_id: int) -> str | None:
    """Build the conversation summary section (compressed older messages)."""
    from app.memory.service import get_working_memory

    wm = get_working_memory(session, conversation_id)
    if wm is None or not wm.summary:
        return None

    summary = wm.summary
    if len(summary) > 1000:
        summary = summary[:1000] + "…"

    return f"[CONVERSATION SUMMARY]\n{summary}"


def _get_working_memory_block(session: Session, *, conversation_id: int) -> str | None:
    """Build the working memory / scratchpad section."""
    from app.memory.service import get_working_memory

    wm = get_working_memory(session, conversation_id)
    if wm is None or not wm.state:
        return None

    state = wm.state
    lines = ["[WORKING MEMORY]"]

    # Current goal.
    if state.get("current_goal"):
        lines.append(f"Goal: {state['current_goal']}")

    # Hypotheses.
    hypotheses = state.get("hypotheses")
    if hypotheses and isinstance(hypotheses, list):
        lines.append("Hypotheses:")
        for h in hypotheses[:5]:
            if isinstance(h, dict):
                lines.append(f"  - {h.get('text', '')} [{h.get('status', '')}]")
            else:
                lines.append(f"  - {h}")

    # Completed steps.
    completed = state.get("completed")
    if completed and isinstance(completed, list):
        lines.append(f"Completed: {', '.join(str(c) for c in completed[:5])}")

    # Next actions.
    next_actions = state.get("next_actions")
    if next_actions and isinstance(next_actions, list):
        lines.append(f"Next: {', '.join(str(a) for a in next_actions[:3])}")

    # Entity states.
    entities = state.get("entities")
    if entities and isinstance(entities, dict):
        lines.append("Entities:")
        for name, info in list(entities.items())[:5]:
            if isinstance(info, dict):
                status = info.get("status", "")
                lines.append(f"  - {name}: {status}")
            else:
                lines.append(f"  - {name}: {info}")

    # Only return if we have more than just the header.
    if len(lines) <= 1:
        return None

    return "\n".join(lines)
