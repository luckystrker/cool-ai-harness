"""Runners: bridge the AgentExecutor loop to a conversation + persistence.

Centralizes the "load history → run loop → persist new messages" choreography
so the SSE route, the WebSocket endpoint, and later the cron-job executor
share a single implementation.
"""

from __future__ import annotations

import json
from collections.abc import AsyncIterator

from sqlmodel import Session

from app.agent import AgentConfig, AgentEvent, AgentExecutor
from app.agent.permissions import PermissionsConfig, merge as merge_permissions
from app.agent.service import append_message, load_history
from app.core.config import get_settings
from app.core.logging import get_logger
from app.models import Message as MessageRow
from app.providers import LLMProvider

log = get_logger(__name__)


async def run_conversation_turn(
    *,
    session: Session,
    conversation_id: int,
    provider: LLMProvider,
    model: str,
    user_input: str | None,
    system_prompt: str | None = None,
    tool_names: list[str] | None = None,
    working_directory: str | None = None,
    conversation_permissions: dict[str, str] | None = None,
    auto_approve: bool = False,
) -> AsyncIterator[AgentEvent]:
    """Run one agent turn against a conversation, persisting messages along the way.

    Persists:
      - the user message is assumed already saved by the caller (so SSE can
        echo it before the loop starts). ``user_input`` here is forwarded to
        the executor's in-memory history, not written to disk.
      - each tool result as a ``tool`` row when the corresponding event fires
      - the final assistant message as one row on the finish event

    Permissions & workdir:
      - ``working_directory`` overrides the global default for this turn.
      - ``conversation_permissions`` are merged with the global defaults
        (Settings.default_tool_permissions) into the effective PermissionsConfig.
      - ``auto_approve`` makes "ask" tools run without prompting (cron/subagents).
    """
    history = load_history(session, conversation_id)

    settings = get_settings()
    effective_permissions: PermissionsConfig = merge_permissions(
        dict(settings.default_tool_permissions), conversation_permissions
    )

    executor = AgentExecutor(
        provider=provider,
        config=AgentConfig(
            model=model,
            system_prompt=system_prompt,
            tool_names=tool_names,
            working_directory=working_directory,
            permissions=effective_permissions,
            auto_approve=auto_approve,
        ),
        history=history,
    )

    # The executor emits one `message` event per loop iteration (per assistant
    # turn). An iteration that requests tools yields a message with tool_calls;
    # a later iteration yields the final text answer. We persist each of them as
    # its own row so the tool-call request is never lost (previously only the
    # last message was saved on `finish`, which dropped the tool_calls when a
    # follow-up text turn overwrote them).
    persisted_last_assistant_id: int | None = None

    async for event in executor.stream(user_input):
        if event.kind == "message":
            content = event.payload.get("content")
            tool_calls = event.payload.get("tool_calls")
            thinking = event.payload.get("thinking")
            # Skip truly empty turns (no text, no tool calls) — nothing to show.
            if content or tool_calls:
                row = append_message(
                    session,
                    conversation_id=conversation_id,
                    role="assistant",
                    content=content,
                    tool_calls=tool_calls,
                    thinking=thinking,
                )
                persisted_last_assistant_id = row.id
        elif event.kind == "tool_result":
            payload = event.payload
            result = payload.get("result") or {}
            # Persist the structured result plus a tool_call_id/name so the
            # reloaded history can reconstruct the provider tool message and
            # the UI can show the result inline with its call.
            append_message(
                session,
                conversation_id=conversation_id,
                role="tool",
                content=result.get("output"),
                tool_result={
                    "tool_call_id": payload.get("id"),
                    "name": payload.get("name"),
                    "result": result,
                },
            )
        elif event.kind == "finish":
            # Attach the aggregated usage to the most recent assistant message.
            usage = event.payload.get("usage")
            if usage and persisted_last_assistant_id is not None:
                row = session.get(MessageRow, persisted_last_assistant_id)
                if row is not None:
                    row.usage = usage
                    session.add(row)
                    session.commit()
        yield event


def serialize_event(event: AgentEvent) -> str:
    """Serialize an AgentEvent to a JSON string for wire transport."""
    return json.dumps(event.to_dict(), default=str, ensure_ascii=False)
