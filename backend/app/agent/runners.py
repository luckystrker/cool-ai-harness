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
from dataclasses import dataclass

from sqlmodel import Session

from app.agent import AgentConfig, AgentEvent, AgentExecutor, AgentLimits, get_default_system_prompt
from app.agent.permissions import PermissionsConfig
from app.agent.permissions import merge as merge_permissions
from app.agent.project_instructions import load_project_instructions
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
from app.observability import inspector_registry
from app.providers import LLMProvider
from app.security.breakpoints import BreakpointsConfig, merge_breakpoints
from app.security.capabilities import CapabilityPolicy
from app.security.capabilities import merge_policy as merge_capability_policy
from app.security.secrets import mask_secrets_in_value

log = get_logger(__name__)

# --- Context assembly budget (item 6) ---
# Maximum characters for ALL injected context blocks combined (project
# instructions + plan + memory + skills). ~20 000 chars ≈ 5 000 tokens.
MAX_INJECTED_CONTEXT_CHARS = 20_000

# Priority weights for budget allocation. When a section is shorter than its
# allocation, the surplus is redistributed proportionally to the others.
_CONTEXT_WEIGHTS: dict[str, float] = {
    "project_instructions": 0.30,
    "plan": 0.25,
    "memory": 0.25,
    "skills": 0.20,
}

# Event kinds that get persisted to the run_events log one row each, in order.
# token/thinking are batched (see _EventLog) to avoid a write-per-token storm;
# these "structural" kinds are flushed immediately because they're the spine of
# a replay and are infrequent enough that batching adds no benefit.
_STRUCTUREAL_KINDS = frozenset(
    {"start", "message", "tool_call_start", "tool_result", "finish", "error",
     "react_thought", "react_action", "react_observation", "llm_call_complete"}
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


# --- Item 10: pipeline stages extracted from run_conversation_turn ---


@dataclass
class _ResolvedTurn:
    """Fully resolved configuration for a conversation turn.

    Produced by :func:`_resolve_turn_config`; consumed by
    :func:`run_conversation_turn`. Each field is independently testable.
    """

    system_prompt: str | None
    tool_names: list[str] | None
    permissions: PermissionsConfig
    capability_policy: CapabilityPolicy
    breakpoints: BreakpointsConfig
    limits: AgentLimits
    # Resolved profile id (for memory agent_id); None when no profile active.
    profile_id: int | None


def _resolve_turn_config(
    session: Session,
    *,
    conversation_id: int,
    system_prompt: str | None,
    tool_names: list[str] | None,
    working_directory: str | None,
    user_input: str | None,
    conversation_permissions: dict[str, str] | None,
    conversation_capability_policy: dict[str, str] | None,
    conversation_breakpoints: list[dict] | None,
    limits: AgentLimits | None,
    profile_id: int | None,
) -> _ResolvedTurn:
    """Resolve all per-turn configuration from layered sources.

    Pipeline: profile → prompt → tools → context injection → security merge.
    Each stage is a pure function of its inputs, making the pipeline testable
    without running the full agent loop.
    """
    settings = get_settings()

    # --- Stage 1: resolve profile ---
    _profile = None
    if profile_id is not None:
        from app.agent.personalities.service import get_profile

        _profile = get_profile(session, profile_id)

    # --- Stage 2: resolve system prompt (per-request > profile > default) ---
    effective_prompt: str | None
    if system_prompt:
        effective_prompt = system_prompt
    elif _profile and _profile.system_prompt:
        effective_prompt = _profile.system_prompt
    else:
        effective_prompt = get_default_system_prompt() or None

    # --- Stage 3: resolve tool whitelist (per-request > profile > all) ---
    effective_tools = tool_names
    if effective_tools is None and _profile and _profile.tool_names:
        effective_tools = _profile.tool_names

    # --- Stage 4: collect + assemble injected context (budget-aware) ---
    _sections: dict[str, str | None] = {}
    _sections["project_instructions"] = load_project_instructions(working_directory)

    from app.agent.planning import build_plan_context

    _sections["plan"] = build_plan_context(session, conversation_id)

    if user_input:
        from app.skills.context import build_skills_context

        _sections["skills"] = build_skills_context(user_input)
    else:
        _sections["skills"] = None

    if settings.memory_enabled:
        from app.memory.context_builder import build_memory_context

        _mem_user = get_or_create_default_user(session)
        assert _mem_user.id is not None
        _sections["memory"] = build_memory_context(
            session,
            user_id=_mem_user.id,
            agent_id=_profile.id if _profile else None,
            conversation_id=conversation_id,
            query=user_input,
        )
    else:
        _sections["memory"] = None

    assembled = _assemble_context_budget(_sections)
    if assembled and effective_prompt:
        effective_prompt = f"{effective_prompt}\n\n{assembled}"
    elif assembled:
        effective_prompt = assembled

    # --- Stage 5: merge security layers (global < profile < conversation) ---
    effective_permissions: PermissionsConfig = merge_permissions(
        dict(settings.default_tool_permissions), conversation_permissions
    )
    _profile_cap_policy = None
    if _profile and _profile.settings and isinstance(_profile.settings, dict):
        _profile_cap_policy = _profile.settings.get("capability_policy")
    effective_capability_policy = merge_capability_policy(
        dict(settings.capability_policy), _profile_cap_policy
    )
    # Pass .caps (dict) so the second merge preserves the first merge's result
    # (passing the CapabilityPolicy object would be treated as non-dict → {}).
    effective_capability_policy = merge_capability_policy(
        effective_capability_policy.caps, conversation_capability_policy
    )
    effective_breakpoints = merge_breakpoints(None, conversation_breakpoints)
    effective_limits = limits or _default_limits(settings)

    return _ResolvedTurn(
        system_prompt=effective_prompt,
        tool_names=effective_tools,
        permissions=effective_permissions,
        capability_policy=effective_capability_policy,
        breakpoints=effective_breakpoints,
        limits=effective_limits,
        profile_id=_profile.id if _profile else None,
    )

def _persist_tool_result(
    session: Session,
    *,
    settings,
    conversation_id: int,
    run_id: int | None,
    payload: dict,
    persisted_last_assistant_id: int | None,
    pending_tool_args: dict[str, dict],
    pending_approval_meta: dict[str, tuple[str, float]],
    approval_requested: set[str],
    iteration_count: int,
) -> None:
    """Persist all rows for a tool_result event in a single transaction (item 7+10).

    Writes: tool message + ToolCallRow + ApprovalAudit + run checkpoint,
    then commits once.
    """
    result = payload.get("result") or {}
    call_id = payload.get("id")

    # 1. Tool message (flush, no commit yet).
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
        commit=False,
    )

    # 2. Observability row.
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

    # 3. Approval audit.
    is_denied = bool(metadata.get("denied"))
    is_breakpoint = bool(metadata.get("breakpoint"))
    bp_type = metadata.get("breakpoint")
    audit_name, audit_t0 = pending_approval_meta.pop(call_id, (payload.get("name", ""), 0.0))
    audit_duration_ms = int((time.monotonic() - audit_t0) * 1000) if audit_t0 else None
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

    # 4. Run checkpoint.
    if run_id is not None:
        update_run(
            session,
            run_id,
            commit=False,
            checkpoint={
                "iteration": iteration_count,
                "last_call_id": call_id,
                "last_tool": payload.get("name"),
            },
        )

    # Single commit for all writes.
    session.commit()


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
    profile_id: int | None = None,
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
    settings = get_settings()
    history = load_history(session, conversation_id)

    # --- Resolve all per-turn configuration (item 10: extracted pipeline) ---
    resolved = _resolve_turn_config(
        session,
        conversation_id=conversation_id,
        system_prompt=system_prompt,
        tool_names=tool_names,
        working_directory=working_directory,
        user_input=user_input,
        conversation_permissions=conversation_permissions,
        conversation_capability_policy=conversation_capability_policy,
        conversation_breakpoints=conversation_breakpoints,
        limits=limits,
        profile_id=profile_id,
    )

    executor = AgentExecutor(
        provider=provider,
        config=AgentConfig(
            model=model,
            system_prompt=resolved.system_prompt,
            tool_names=resolved.tool_names,
            limits=resolved.limits,
            working_directory=working_directory,
            permissions=resolved.permissions,
            capability_policy=resolved.capability_policy,
            breakpoints=resolved.breakpoints,
            auto_approve=auto_approve,
            run_id=run_id,
            cancellable=cancellable,
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
            config=_limits_to_config(resolved.limits, tool_names),
        )

    # --- Mutable loop state ---
    persisted_last_assistant_id: int | None = None
    pending_tool_args: dict[str, dict] = {}
    pending_approval_meta: dict[str, tuple[str, float]] = {}
    approval_requested: set[str] = set()
    iteration_count = 0
    terminal_reason: str | None = None
    turn_t0 = time.monotonic()

    try:
        async for event in executor.stream(user_input):
            if event_log is not None:
                event_log.add(event.kind, dict(event.payload) if event.payload else None)

            if event.kind == "message":
                content = event.payload.get("content")
                tool_calls = event.payload.get("tool_calls")
                thinking = event.payload.get("thinking")
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
                call_id = event.payload.get("id")
                if call_id is not None:
                    pending_tool_args[call_id] = event.payload.get("arguments") or {}
                    pending_approval_meta[call_id] = (
                        event.payload.get("name", ""),
                        time.monotonic(),
                    )

            elif event.kind == "tool_approval_request":
                call_id = event.payload.get("id")
                if call_id is not None:
                    approval_requested.add(call_id)
                    if call_id not in pending_approval_meta:
                        pending_approval_meta[call_id] = (
                            event.payload.get("name", ""),
                            time.monotonic(),
                        )

            elif event.kind == "tool_result":
                _persist_tool_result(
                    session,
                    settings=settings,
                    conversation_id=conversation_id,
                    run_id=run_id,
                    payload=event.payload,
                    persisted_last_assistant_id=persisted_last_assistant_id,
                    pending_tool_args=pending_tool_args,
                    pending_approval_meta=pending_approval_meta,
                    approval_requested=approval_requested,
                    iteration_count=iteration_count,
                )

            elif event.kind == "finish":
                terminal_reason = event.payload.get("reason")
                usage = event.payload.get("usage")
                if persisted_last_assistant_id is not None:
                    row = session.get(MessageRow, persisted_last_assistant_id)
                    if row is not None:
                        if usage:
                            row.usage = usage
                        row.model = model
                        row.duration_ms = int((time.monotonic() - turn_t0) * 1000)
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

            # Inspector live-feed: publish every event to subscribers (Фаза 1.5 §6).
            if run_id is not None:
                inspector_registry.publish(run_id, event.to_dict())
    finally:
        if event_log is not None:
            event_log.flush()

        # History consistency: backfill placeholder tool results for any
        # tool_calls that never completed (cancel/error mid-batch).
        if persisted_last_assistant_id is not None:
            _backfill_missing_tool_results(session, conversation_id, persisted_last_assistant_id)

        if run_id is not None:
            from app.agent.service import get_run

            run = get_run(session, run_id)
            if run is not None and run.status not in (
                "completed",
                "failed",
                "cancelled",
            ):
                reason = (
                    "cancelled" if (cancellable and run_registry.is_cancelled(run_id)) else "error"
                )
                finish_run(session, run_id, finish_reason=reason, iterations=iteration_count)

            # Notify inspector subscribers that the run has ended.
            inspector_registry.notify_finished(run_id)


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


def _assemble_context_budget(sections: dict[str, str | None]) -> str | None:
    """Assemble context sections with priority-weighted budget allocation.

    Each section gets a character budget proportional to its weight. If a
    section is shorter than its allocation, the surplus is redistributed to
    the remaining sections proportionally. Sections that exceed their final
    budget are truncated (at a line boundary when possible).

    Returns the concatenated context string, or None if all sections are empty.
    """
    # Filter to non-empty sections.
    active: dict[str, str] = {k: v for k, v in sections.items() if v}
    if not active:
        return None

    total_budget = MAX_INJECTED_CONTEXT_CHARS

    # Check if everything fits without truncation.
    total_chars = sum(len(v) for v in active.values())
    if total_chars <= total_budget:
        return "\n\n".join(active.values())

    # Allocate budget proportionally, redistributing surplus.
    # Iterative: sections that fit within their allocation free up budget
    # for the others.
    allocations: dict[str, int] = {}
    remaining_keys = set(active.keys())
    remaining_budget = total_budget

    for _pass in range(3):  # converges in ≤ 3 passes for 4 sections
        if not remaining_keys:
            break
        # Compute weights for remaining sections.
        weight_sum = sum(_CONTEXT_WEIGHTS.get(k, 0.2) for k in remaining_keys)
        if weight_sum <= 0:
            # Equal split fallback.
            per_section = remaining_budget // len(remaining_keys)
            for k in remaining_keys:
                allocations[k] = per_section
            break

        settled_this_pass: list[str] = []
        for key in remaining_keys:
            weight = _CONTEXT_WEIGHTS.get(key, 0.2)
            alloc = int(remaining_budget * (weight / weight_sum))
            if len(active[key]) <= alloc:
                # Section fits — allocate its actual size, free the surplus.
                allocations[key] = len(active[key])
                settled_this_pass.append(key)

        if not settled_this_pass:
            # No section fits — allocate proportionally and truncate all.
            for key in remaining_keys:
                weight = _CONTEXT_WEIGHTS.get(key, 0.2)
                allocations[key] = int(remaining_budget * (weight / weight_sum))
            break

        # Remove settled sections and recalculate.
        for key in settled_this_pass:
            remaining_budget -= allocations[key]
            remaining_keys.discard(key)

    # Assemble with truncation.
    parts: list[str] = []
    for key in ("project_instructions", "plan", "memory", "skills"):
        content = active.get(key)
        if not content:
            continue
        budget = allocations.get(key, len(content))
        if len(content) > budget:
            content = _truncate_at_boundary(content, budget)
            log.info(
                "context_budget.truncated_section",
                section=key,
                original_chars=len(active[key]),
                budget=budget,
            )
        parts.append(content)

    return "\n\n".join(parts) if parts else None


def _truncate_at_boundary(text: str, max_chars: int) -> str:
    """Truncate text to max_chars, preferring a line boundary."""
    if len(text) <= max_chars:
        return text
    truncated = text[:max_chars]
    last_newline = truncated.rfind("\n")
    if last_newline > max_chars // 2:  # only if we keep at least half
        truncated = truncated[:last_newline]
    return truncated + "\n… (truncated)"


def _backfill_missing_tool_results(
    session: Session, conversation_id: int, assistant_msg_id: int
) -> None:
    """Persist placeholder tool results for any tool_calls that never completed.

    When a run is cancelled or errors mid-tool-batch, the assistant message
    already references N tool_calls but only M < N tool result rows exist.
    LLM providers require a tool response for every tool_call in the preceding
    assistant message; without this the next turn's API call fails or the model
    produces garbled output. This helper fills the gaps with a clear
    "cancelled" indicator so the history stays well-formed.
    """
    from sqlmodel import select

    assistant_row = session.get(MessageRow, assistant_msg_id)
    if assistant_row is None or not assistant_row.tool_calls:
        return

    # Find tool result messages that follow this assistant message.
    subsequent_tool_rows = session.exec(
        select(MessageRow)
        .where(MessageRow.conversation_id == conversation_id)
        .where(MessageRow.id > assistant_msg_id)
        .where(MessageRow.role == "tool")
    ).all()
    answered_call_ids: set[str] = set()
    for row in subsequent_tool_rows:
        if row.tool_result and row.tool_result.get("tool_call_id"):
            answered_call_ids.add(row.tool_result["tool_call_id"])

    # Persist a placeholder for each unanswered tool call.
    for tc in assistant_row.tool_calls:
        call_id = tc.get("id") or ""
        if call_id and call_id not in answered_call_ids:
            append_message(
                session,
                conversation_id=conversation_id,
                role="tool",
                content=f"[Cancelled] Tool '{tc.get('name', 'unknown')}' did not complete.",
                tool_result={
                    "tool_call_id": call_id,
                    "name": tc.get("name", ""),
                    "result": {
                        "output": f"[Cancelled] Tool '{tc.get('name', 'unknown')}' did not complete.",
                        "is_error": True,
                        "error": "Run cancelled before tool execution completed.",
                    },
                },
            )
            answered_call_ids.add(call_id)
