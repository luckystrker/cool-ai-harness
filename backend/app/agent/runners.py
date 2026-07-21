"""Runners: bridge the AgentExecutor loop to a conversation + persistence.

Centralizes the "load history → run loop → persist new messages" choreography
so the SSE route, the WebSocket endpoint, and later the cron-job executor
share a single implementation.
"""

from __future__ import annotations

import json
from collections.abc import AsyncIterator
from typing import Any

from sqlmodel import Session

from app.agent import AgentConfig, AgentEvent, AgentExecutor
from app.agent.service import append_message, load_history
from app.core.logging import get_logger
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
) -> AsyncIterator[AgentEvent]:
    """Run one agent turn against a conversation, persisting messages along the way.

    Persists:
      - the user message is assumed already saved by the caller (so SSE can
        echo it before the loop starts). ``user_input`` here is forwarded to
        the executor's in-memory history, not written to disk.
      - each tool result as a ``tool`` row when the corresponding event fires
      - the final assistant message as one row on the finish event
    """
    history = load_history(session, conversation_id)
    executor = AgentExecutor(
        provider=provider,
        config=AgentConfig(
            model=model,
            system_prompt=system_prompt,
            tool_names=tool_names,
        ),
        history=history,
    )

    assistant_content: list[str] = []
    last_tool_calls: list[dict[str, Any]] | None = None

    async for event in executor.stream(user_input):
        if event.kind == "tool_result":
            payload = event.payload
            append_message(
                session,
                conversation_id=conversation_id,
                role="tool",
                content=payload["result"]["output"],
            )
        elif event.kind == "token":
            assistant_content.append(event.payload.get("text", ""))
        elif event.kind == "message":
            last_tool_calls = event.payload.get("tool_calls")
        elif event.kind == "finish":
            content = "".join(assistant_content) or None
            if content or last_tool_calls:
                append_message(
                    session,
                    conversation_id=conversation_id,
                    role="assistant",
                    content=content,
                    tool_calls=last_tool_calls,
                    usage=event.payload.get("usage"),
                )
        yield event


def serialize_event(event: AgentEvent) -> str:
    """Serialize an AgentEvent to a JSON string for wire transport."""
    return json.dumps(event.to_dict(), default=str, ensure_ascii=False)
