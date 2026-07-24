"""Runners: bridge the AgentExecutor loop to a conversation + persistence.

Centralizes the "load history → run loop → persist new messages" choreography
so the SSE route, the WebSocket endpoint, and later the cron-job executor
share a single implementation.

As of Фаза 1.5, a run can be *durable*: an ``AgentRun`` row tracks its status,
usage, iterations and outcome, an append-only ``run_events`` log records every
event for replay/inspection, and an interactive (``cancellable``) run can be
stopped via the run registry.
"""

from __future__ import annotations

import json
import time
from collections.abc import AsyncIterator

from sqlmodel import Session

from app.agent import AgentConfig, AgentEvent, AgentExecutor, AgentLimits, get_default_system_prompt
from app.agent.permissions import PermissionsConfig
from app.agent.permissions import merge as merge_permissions
from app.agent.runs import run_registry
from app.agent.service import (
    append_message,
    append_run_events,
    finish_run,
    get_or_create_default_user,
    load_history,
    update_run,
)
from app.core.config import get_settings
from app.core.logging import get_logger
from app.models import ApprovalAudit
from app.models import Message as MessageRow
from app.models import ToolCall as ToolCallRow
from app.models.run import RUN_STATUS_RUNNING
from app.providers import LLMProvider
from app.security.breakpoints import merge_breakpoints
from app.security.capabilities import merge_policy as merge_capability_policy
from app.security.secrets import mask_secrets_in_value

log = get_logger(__name__)

# Event kinds that get persisted to the run_events log one row each, in order.
# token/thinking are batched (see _EventLog) to avoid a write-per-token storm;
# these "structural" kinds are flushed immediately because they're the spine of
# a replay and are infrequent enough that batching adds no benefit.
_STRUCTUREAL_KINDS = frozenset(
    {"start", "message", "tool_call_start", "tool_result", "finish", "error",
     "react_thought", "react_action", "react_observation"}
)


class _EventLog:
    """Buffers streamed (token/thinking) events and flushes them in batches.

    The run_events table is an append-only log of *every* event, but writing a
    row per token is wasteful. We accumulate token/thinking payloads and flush
    them together when a structural event arrives (or the run ends), preserving
    order via ``seq``.
    """

    def __init__(self, session: Session, run_id: int) -> None:
        self._session = session
        self._run_id = run_id
        self._buffer: list[tuple[str, dict | None]] = []

    def add(self, kind: str, payload: dict | None) -> None:
        """Queue an event. Structural kinds trigger an immediate flush first."""
        if kind in _STRUCTUREAL_KINDS:
            self.flush()
            append_run_events(self._session, run_id=self._run_id, events=[(kind, payload)])
        else:
            self._buffer.append((kind, payload))

    def flush(self) -> None:
        """Write any buffered streamed events, in order, as one batch."""
        if not self._buffer:
            return
        append_run_events(self._session, run_id=self._run_id, events=self._buffer)
        self._buffer.clear()


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
    conversation_capability_policy: dict[str, str] | None = None,
    conversation_breakpoints: list[dict] | None = None,
    auto_approve: bool = False,
    limits: AgentLimits | None = None,
    run_id: int | None = None,
    cancellable: bool = False,
) -> AsyncIterator[AgentEvent]:
    """Run one agent turn against a conversation, persisting messages along the way.

    Persists:
      - the user message is assumed already saved by the caller (so SSE can
        echo it before the loop starts). ``user_input`` here is forwarded to
        the executor's in-memory history, not written to disk.
      - each tool result as a ``tool`` row when the corresponding event fires,
        plus an observability ``tool_calls`` row capturing name/arguments/
        result/duration/success (Фаза 3a prep).
      - the final assistant message as one row on the finish event
      - when ``run_id`` is set: an append-only ``run_events`` log of every event,
        plus progress updates to the ``agent_runs`` row (status/iterations/
        usage/checkpoint/finish_reason).

    Permissions & workdir:
      - ``working_directory`` overrides the global default for this turn.
      - ``conversation_permissions`` are merged with the global defaults
        (Settings.default_tool_permissions) into the effective PermissionsConfig.
      - ``auto_approve`` makes "ask" tools run without prompting (cron/subagents).

    Durable runs (Фаза 1.5):
      - ``run_id`` ties this turn to an AgentRun row (created by the caller via
        ``service.create_run``). When set, events are logged and the run row is
        kept in sync; when None, the turn is unmanaged (legacy behavior).
      - ``cancellable`` (requires run_id) registers the run for cancellation;
        the executor polls the registry each iteration / before each tool call.
    """
    history = load_history(session, conversation_id)

    settings = get_settings()

    # Resolve the effective system prompt: per-request > settings default > built-in file.
    effective_system_prompt = system_prompt or get_default_system_prompt() or None

    effective_permissions: PermissionsConfig = merge_permissions(
        dict(settings.default_tool_permissions), conversation_permissions
    )
    effective_capability_policy = merge_capability_policy(
        dict(settings.capability_policy), conversation_capability_policy
    )
    effective_breakpoints = merge_breakpoints(
        None, conversation_breakpoints  # Global breakpoints from settings (future)
    )
    effective_limits = limits or _default_limits(settings)

    executor = AgentExecutor(
        provider=provider,
        config=AgentConfig(
            model=model,
            system_prompt=effective_system_prompt,
            tool_names=tool_names,
            limits=effective_limits,
            working_directory=working_directory,
            permissions=effective_permissions,
            capability_policy=effective_capability_policy,
            breakpoints=effective_breakpoints,
            auto_approve=auto_approve,
            run_id=run_id,
            cancellable=cancellable,
            # Cost-budget accounting (Фаза 1.5 §5): single-user MVP.
            user_id=get_or_create_default_user(session).id,
            conversation_id=conversation_id,
        ),
        history=history,
    )

    # Register for cancellation before the loop starts so a cancel racing with
    # startup is observed. Unregistered (non-cancellable) runs skip this.
    if cancellable and run_id is not None:
        run_registry.register(run_id, conversation_id=conversation_id)

    event_log = _EventLog(session, run_id) if run_id is not None else None
    if run_id is not None:
        update_run(
            session,
            run_id,
            status=RUN_STATUS_RUNNING,
            config=_limits_to_config(effective_limits, tool_names),
        )

    # The executor emits one `message` event per loop iteration (per assistant
    # turn). An iteration that requests tools yields a message with tool_calls;
    # a later iteration yields the final text answer. We persist each of them as
    # its own row so the tool-call request is never lost (previously only the
    # last message was saved on `finish`, which dropped the tool_calls when a
    # follow-up text turn overwrote them).
    persisted_last_assistant_id: int | None = None
    # tool_call_id -> arguments dict. Captured from tool_call_start so the
    # ToolCall observability row (written on tool_result) can record what was
    # actually invoked, not just the result.
    pending_tool_args: dict[str, dict] = {}
    # tool_call_id -> (name, t0_monotonic) for approval audit timing.
    pending_approval_meta: dict[str, tuple[str, float]] = {}
    # call_ids that had an explicit approval request (vs auto-allowed).
    approval_requested: set[str] = set()
    # Count of completed LLM iterations for the run row.
    iteration_count = 0
    terminal_reason: str | None = None

    try:
        async for event in executor.stream(user_input):
            if event_log is not None:
                event_log.add(event.kind, dict(event.payload) if event.payload else None)

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
                iteration_count += 1
                if run_id is not None:
                    update_run(session, run_id, iterations=iteration_count)
            elif event.kind == "tool_call_start":
                # Remember the arguments so the observability row (written when
                # the tool finishes) can record them.
                call_id = event.payload.get("id")
                if call_id is not None:
                    pending_tool_args[call_id] = event.payload.get("arguments") or {}
                    pending_approval_meta[call_id] = (
                        event.payload.get("name", ""),
                        time.monotonic(),
                    )
            elif event.kind == "tool_approval_request":
                # Track that an approval was requested so we can audit it when
                # the tool_result arrives (approved or denied).
                call_id = event.payload.get("id")
                if call_id is not None:
                    approval_requested.add(call_id)
                    if call_id not in pending_approval_meta:
                        pending_approval_meta[call_id] = (
                            event.payload.get("name", ""),
                            time.monotonic(),
                        )
            elif event.kind == "tool_result":
                payload = event.payload
                result = payload.get("result") or {}
                call_id = payload.get("id")
                # Persist the structured result plus a tool_call_id/name so the
                # reloaded history can reconstruct the provider tool message and
                # the UI can show the result inline with its call.
                append_message(
                    session,
                    conversation_id=conversation_id,
                    role="tool",
                    content=result.get("output"),
                    tool_result={
                        "tool_call_id": call_id,
                        "name": payload.get("name"),
                        "result": result,
                    },
                )
                # Observability: one row per tool invocation (Фаза 3a prep).
                metadata = result.get("metadata") or {}
                tool_args = pending_tool_args.pop(call_id, None)
                session.add(
                    ToolCallRow(
                        conversation_id=conversation_id,
                        message_id=persisted_last_assistant_id,
                        name=payload.get("name", ""),
                        arguments=tool_args,
                        result=result,
                        duration_ms=metadata.get("duration_ms"),
                        success=not result.get("is_error", False),
                        error=result.get("error"),
                    )
                )
                # Approval audit: record the decision (approved/denied/auto).
                # If the tool was denied, the metadata carries {"denied": True}.
                is_denied = bool(metadata.get("denied"))
                is_breakpoint = bool(metadata.get("breakpoint"))
                bp_type = metadata.get("breakpoint")
                audit_name, audit_t0 = pending_approval_meta.pop(call_id, (payload.get("name", ""), 0.0))
                audit_duration_ms = int((time.monotonic() - audit_t0) * 1000) if audit_t0 else None
                # Determine the decision source:
                #   - "timeout": approval was requested but timed out (auto-denied)
                #   - "user":    approval was requested and resolved by the user
                #   - "policy":  denied by capability/permission policy (no user)
                #   - "auto":    allowed without asking (auto_approve or allow)
                if is_denied and call_id in approval_requested:
                    decision_source = "timeout"
                elif call_id in approval_requested:
                    decision_source = "user"
                elif is_denied:
                    decision_source = "policy"
                else:
                    decision_source = "auto"
                approval_requested.discard(call_id)
                session.add(
                    ApprovalAudit(
                        conversation_id=conversation_id,
                        run_id=run_id,
                        call_id=call_id or "",
                        tool_name=audit_name,
                        arguments=mask_secrets_in_value(
                            tool_args or {},
                            enabled=settings.mask_secrets,
                        ),
                        approved=not is_denied,
                        decision_source=decision_source,
                        decided_by="default",
                        reason=f"Breakpoint: {bp_type}" if is_breakpoint else None,
                        is_breakpoint=is_breakpoint,
                        breakpoint_type=bp_type if is_breakpoint else None,
                        duration_ms=audit_duration_ms,
                    )
                )
                session.commit()
                # Checkpoint: record the last completed tool call so a future
                # resume knows where the loop got to.
                if run_id is not None:
                    update_run(
                        session,
                        run_id,
                        checkpoint={
                            "iteration": iteration_count,
                            "last_call_id": call_id,
                            "last_tool": payload.get("name"),
                        },
                    )
            elif event.kind == "finish":
                terminal_reason = event.payload.get("reason")
                usage = event.payload.get("usage")
                # Attach the aggregated usage to the most recent assistant message.
                if usage and persisted_last_assistant_id is not None:
                    row = session.get(MessageRow, persisted_last_assistant_id)
                    if row is not None:
                        row.usage = usage
                        session.add(row)
                        session.commit()
                if run_id is not None:
                    if event_log is not None:
                        event_log.flush()
                    finish_run(
                        session,
                        run_id,
                        finish_reason=terminal_reason or "stop",
                        usage=usage,
                        iterations=event.payload.get("iterations", iteration_count),
                    )
            elif event.kind == "error":
                if run_id is not None:
                    if event_log is not None:
                        event_log.flush()
                    finish_run(
                        session,
                        run_id,
                        finish_reason="error",
                        error=event.payload.get("detail") or event.payload.get("message"),
                        iterations=iteration_count,
                    )
            yield event
    finally:
        # If the loop exited without a terminal event (e.g. an unexpected
        # exception escaped the executor, or the runner was cancelled itself),
        # make sure the run row doesn't stay "running" forever.
        if event_log is not None:
            event_log.flush()
        if run_id is not None:
            from app.agent.service import get_run

            run = get_run(session, run_id)
            if run is not None and run.status not in (
                "completed",
                "failed",
                "cancelled",
            ):
                # Cancellation takes precedence: if the registry says it was
                # cancelled, record that; otherwise treat as a failure.
                reason = (
                    "cancelled" if (cancellable and run_registry.is_cancelled(run_id)) else "error"
                )
                finish_run(session, run_id, finish_reason=reason, iterations=iteration_count)


def serialize_event(event: AgentEvent) -> str:
    """Serialize an AgentEvent to a JSON string for wire transport."""
    return json.dumps(event.to_dict(), default=str, ensure_ascii=False)


def _default_limits(settings) -> AgentLimits:
    """Build AgentLimits from settings, honoring None = no ceiling."""
    return AgentLimits(
        max_iterations=settings.agent_max_iterations,
        max_total_tokens=settings.agent_max_total_tokens,
        max_cost_usd=settings.agent_max_cost_usd,
    )


def _limits_to_config(limits: AgentLimits, tool_names: list[str] | None) -> dict:
    """Snapshot the limits + tool whitelist into the run's config JSON."""
    return {
        "max_iterations": limits.max_iterations,
        "max_total_tokens": limits.max_total_tokens,
        "max_cost_usd": limits.max_cost_usd,
        "tool_names": tool_names,
    }
