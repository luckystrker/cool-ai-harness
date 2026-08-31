"""AgentExecutor — the core agent loop.

Loop shape (with tool calling):
    1. Stream a completion from the LLM with the conversation + tool specs.
       Text deltas are forwarded to the caller as ``token`` events. Tool-call
       argument fragments are accumulated locally until the stream ends.
    2. After the assistant turn completes:
       - If tool calls were requested: execute each, emit ``tool_call_start``
         and ``tool_result`` events, append a ``tool`` message to the history,
         and loop back to step 1.
       - Otherwise: emit ``finish`` and stop.
    3. Stop early if a limit (iterations / tokens / cost) is hit.

Keeping the loop transport-agnostic (it just yields AgentEvents) means the
same code drives the chat UI, subagents (Фаза 2), and cron jobs (Фаза 3b).
"""

from __future__ import annotations

import json
import time
from collections.abc import AsyncIterator
from dataclasses import dataclass, field
from typing import Any

from sqlmodel import Session, select

from app.agent.approvals import DEFAULT_APPROVAL_TIMEOUT_S, approval_registry
from app.agent.context_window import compute_history_budget, truncate_history
from app.agent.events import AgentEvent
from app.agent.permissions import Decision, PermissionsConfig
from app.agent.runs import run_registry
from app.budgets import (
    budget_evaluation,
    mark_alert_fired,
    record_spend,
    window_start,
)
from app.core.db import engine
from app.core.logging import get_logger
from app.models import Budget
from app.providers import (
    LLMProvider,
    Message,
    ToolSpec,
    Usage,
)
from app.security.breakpoints import BreakpointsConfig, BreakpointType, is_write_tool
from app.security.capabilities import CapabilityPolicy, stricter
from app.security.secrets import mask_secrets_in_value
from app.tools import ToolResult, get_tool
from app.tools.context import RunContext, get_run_context, reset_run_context, set_run_context

log = get_logger(__name__)


@dataclass
class AgentLimits:
    """Safety limits for a single run."""

    max_iterations: int = 10
    max_total_tokens: int | None = None
    max_cost_usd: float | None = None


@dataclass
class AgentConfig:
    """Per-run configuration."""

    model: str
    system_prompt: str | None = None
    temperature: float = 0.7
    max_tokens: int | None = None
    # Whitelist of tool names exposed to the model. None = all registered tools.
    tool_names: list[str] | None = None
    limits: AgentLimits = field(default_factory=AgentLimits)
    # Working directory for file/code tools this run. None = global default.
    working_directory: str | None = None
    # Effective tool permissions (global + conversation already merged).
    # None means "no explicit config" → resolve() falls back to "ask".
    permissions: PermissionsConfig | None = None
    # Effective capability policy (global + conversation already merged).
    # None means no capability gating (opt-in). See app/security/capabilities.py.
    capability_policy: CapabilityPolicy | None = None
    # Effective breakpoint config (global + conversation already merged).
    # None means no breakpoints. See app/security/breakpoints.py.
    breakpoints: BreakpointsConfig | None = None
    # When True, "ask" tools run without prompting (non-interactive runners:
    # cron jobs, subagents). The approval event is never emitted.
    auto_approve: bool = False
    # Durable-run identity (Фаза 1.5). When set, the start event carries it and
    # the loop checks the run registry for cancellation. None keeps the legacy
    # (unmanaged) behavior — used by tests and non-cancellable runners.
    run_id: int | None = None
    # When True (and run_id is set), the loop polls run_registry for a cancel
    # signal each iteration / before each tool call. Interactive runs (SSE/WS)
    # set this; cron/subagents leave it False.
    cancellable: bool = False
    # --- Cost budgets (Фаза 1.5 §5) ---
    # Identity used to attribute per-call spend and enforce the user's budget.
    # When user_id is None, budget enforcement is skipped (tests / non-managed
    # runners). conversation_id is recorded on each spend row for the UI.
    user_id: int | None = None
    conversation_id: int | None = None
    # Active agent role/personality id — drives agent-scoped memory visibility
    # for tools (memory_recall) and subagent attribution.
    agent_id: int | None = None
    # Skill whitelist from an Agent Constructor blueprint. None = all.
    skill_names: list[str] | None = None


class AgentExecutor:
    """Runs a single agent turn (possibly multiple LLM round-trips for tools).

    History is mutable; callers can read ``executor.history`` after the run
    to persist the full conversation (including tool messages).
    """

    def __init__(
        self,
        *,
        provider: LLMProvider,
        config: AgentConfig,
        history: list[Message] | None = None,
    ) -> None:
        self.provider = provider
        self.config = config
        self.history: list[Message] = list(history or [])
        if config.system_prompt and not any(m.role == "system" for m in self.history):
            self.history.insert(0, Message(role="system", content=config.system_prompt))

    # ---- public API ----

    def available_tools(self) -> list[ToolSpec]:
        """ToolSpecs for whitelisted (or all) registered tools."""
        names = (
            list(_all_tool_names())
            if self.config.tool_names is None
            else self.config.tool_names
        )
        specs: list[ToolSpec] = []
        for name in names:
            tool = get_tool(name)
            if tool is None:
                continue
            specs.append(
                ToolSpec(
                    name=tool.name,
                    description=tool.description,
                    parameters=tool.parameters_schema(),
                )
            )
        return specs

    async def stream(self, user_input: str | None) -> AsyncIterator[AgentEvent]:
        """Run the loop, yielding events.

        ``user_input`` (if given) is appended as a user message before the run.
        Persistence is the caller's responsibility; the executor only mutates
        its own ``history``.
        """
        limits = self.config.limits
        tools = self.available_tools()
        if user_input is not None:
            self.history.append(Message(role="user", content=user_input))

        total_usage = Usage()
        run_started = time.monotonic()

        # Install the run's execution context so file/code tools pick up the
        # per-run working directory and permissions. Reset on exit so a later
        # run (same task) starts clean.
        ctx = self._build_run_context()
        ctx_token = set_run_context(ctx)

        try:
            # Honor a cancel that arrived before the loop even started.
            if self._is_cancelled():
                yield AgentEvent.finish(
                    reason="cancelled",
                    usage=total_usage,
                    iterations=0,
                    elapsed_ms=_elapsed_ms(run_started),
                )
                return

            yield AgentEvent.start(run_id=self.config.run_id)

            react_step = 0  # ReAct step counter (Thought→Action→Observation)

            for iteration in range(1, limits.max_iterations + 1):
                # Check for cancellation at the top of every iteration so a
                # cancel mid-tool-loop stops before the next LLM round-trip.
                if self._is_cancelled():
                    yield AgentEvent.finish(
                        reason="cancelled",
                        usage=total_usage,
                        iterations=iteration - 1,
                        elapsed_ms=_elapsed_ms(run_started),
                    )
                    return

                iter_t0 = time.monotonic()
                content_parts: list[str] = []
                reasoning_parts: list[str] = []
                tool_calls: list[dict[str, Any]] = []
                usage: Usage | None = None
                finish_reason: str | None = None

                # --- Cost budget pre-call gate (Фаза 1.5 §5) ---
                # Block new LLM calls once the user's budget is exceeded
                # (unless an explicit override is active). Evaluated each
                # iteration so a mid-run overrun (e.g. a long tool loop) still
                # stops before the next charge. Skipped when user_id is unset
                # (tests / non-managed runners).
                if self.config.user_id is not None:
                    try:
                        with Session(engine) as _budget_session:
                            _eval = budget_evaluation(_budget_session, user_id=self.config.user_id)
                    except Exception as exc:
                        log.warning("agent.budget_check_failed", error=str(exc))
                        _eval = None
                    if _eval is not None and _eval.blocked:
                        yield AgentEvent.finish(
                            reason="budget_exceeded",
                            usage=total_usage,
                            iterations=iteration - 1,
                            elapsed_ms=_elapsed_ms(run_started),
                        )
                        return

                # --- Context window management ---
                # Truncate history to fit within the model's context window
                # before each LLM call. Preserves system prompt and recent
                # messages; drops oldest exchanges atomically.
                from app.core.config import get_settings as _get_settings

                _ctx_settings = _get_settings()
                _history_budget = compute_history_budget(
                    _ctx_settings.context_window_tokens,
                    _ctx_settings.context_reserve_ratio,
                )
                llm_history = truncate_history(self.history, max_tokens=_history_budget)

                try:
                    async for event in self.provider.chat_completion_stream(
                        llm_history,
                        model=self.config.model,
                        tools=tools or None,
                        temperature=self.config.temperature,
                        max_tokens=self.config.max_tokens,
                    ):
                        if event.reasoning:
                            reasoning_parts.append(event.reasoning)
                            yield AgentEvent.thinking(event.reasoning)
                        if event.delta:
                            content_parts.append(event.delta)
                            yield AgentEvent.token(event.delta)
                        if event.tool_call_delta:
                            _merge_tool_call_deltas(tool_calls, event.tool_call_delta)
                        if event.usage:
                            usage = event.usage
                        if event.finish:
                            finish_reason = event.finish_reason or "stop"
                except Exception as exc:
                    log.error("agent.iteration_failed", iteration=iteration, error=str(exc))
                    yield AgentEvent.error(f"LLM error on iteration {iteration}", detail=str(exc))
                    return

                if usage:
                    _accumulate(total_usage, usage)
                    # Record this call's spend and check budget alerts (Фаза 1.5
                    # §5). Spend is recorded against the per-call delta. Alert
                    # firing is debounced per period via last_alert_at.
                    if self.config.user_id is not None:
                        for ev in self._record_spend_and_maybe_alert(usage):
                            yield ev

                # Inspector: emit per-iteration metrics (Фаза 1.5 §6).
                yield AgentEvent.llm_call_complete(
                    iteration=iteration,
                    model=self.config.model,
                    usage=vars(usage) if usage else None,
                    duration_ms=_elapsed_ms(iter_t0),
                )

                content = "".join(content_parts) or None
                thinking = "".join(reasoning_parts) or None
                # Normalize tool_calls: parse JSON-string arguments into dicts.
                normalized_calls = (
                    [_normalize_tool_call(c) for c in tool_calls] if tool_calls else None
                )

                self.history.append(
                    Message(
                        role="assistant",
                        content=content,
                        tool_calls=normalized_calls,
                    )
                )
                yield AgentEvent.message(
                    content=content,
                    tool_calls=normalized_calls,
                    thinking=thinking,
                )

                # Enforce ceilings.
                if (
                    limits.max_total_tokens is not None
                    and total_usage.total_tokens >= limits.max_total_tokens
                ):
                    yield AgentEvent.finish(
                        reason="token_limit",
                        usage=total_usage,
                        iterations=iteration,
                        elapsed_ms=_elapsed_ms(run_started),
                    )
                    return

                # Per-run cost ceiling (Фаза 1.5 §1/§5). cost_usd is now
                # populated by the pricing table (app/providers/pricing.py);
                # this guard is inert only for unpriced models.
                if (
                    limits.max_cost_usd is not None
                    and (total_usage.cost_usd or 0.0) >= limits.max_cost_usd
                ):
                    yield AgentEvent.finish(
                        reason="cost_limit",
                        usage=total_usage,
                        iterations=iteration,
                        elapsed_ms=_elapsed_ms(run_started),
                    )
                    return

                if not normalized_calls:
                    yield AgentEvent.finish(
                        reason=finish_reason or "stop",
                        usage=total_usage,
                        iterations=iteration,
                        elapsed_ms=_elapsed_ms(run_started),
                    )
                    return

                # --- ReAct: emit Thought → Action → Observation per tool call ---
                # Each tool call gets its own ReAct step so the trace follows
                # the proper cyclical pattern: Thought(1)→Action(1)→Observation(1)
                # → Thought(2)→Action(2)→Observation(2) → …
                # The first step carries the LLM's original reasoning; subsequent
                # steps (from the same LLM response) get a brief continuation note.
                thought_text = thinking or content or "Deciding to use tool(s) to proceed."

                # Execute tool calls, append results, continue the loop.
                # One more cancel check first — a cancel that arrived while we
                # were streaming shouldn't trigger another tool execution.
                if self._is_cancelled():
                    yield AgentEvent.finish(
                        reason="cancelled",
                        usage=total_usage,
                        iterations=iteration,
                        elapsed_ms=_elapsed_ms(run_started),
                    )
                    return

                for call_idx, call in enumerate(normalized_calls):
                    react_step += 1
                    # --- ReAct: emit Thought phase for this step ---
                    if call_idx == 0:
                        yield AgentEvent.react_thought(step=react_step, text=thought_text)
                    else:
                        yield AgentEvent.react_thought(
                            step=react_step,
                            text=f"Continuing with next tool call ({call_idx + 1}/{len(normalized_calls)}).",
                        )
                    # --- ReAct: emit Action phase ---
                    yield AgentEvent.react_action(
                        step=react_step,
                        tool_name=call.get("name", ""),
                        arguments=call.get("arguments") or {},
                        call_id=call.get("id") or call.get("name") or "call",
                    )
                    async for ev in self._run_tool_call(call):
                        tool_limit_reason: str | None = None
                        # --- ReAct: emit Observation after tool_result ---
                        if ev.kind == "tool_result":
                            result_data = ev.payload.get("result") or {}
                            output = result_data.get("output") or ""
                            summary = output[:300] + ("…" if len(output) > 300 else "")
                            yield AgentEvent.react_observation(
                                step=react_step,
                                tool_name=ev.payload.get("name", ""),
                                result_summary=summary,
                                is_error=bool(result_data.get("is_error")),
                            )
                            metadata = result_data.get("metadata") or {}
                            usage_data = metadata.get("llm_usage")
                            if isinstance(usage_data, dict):
                                tool_usage = Usage(
                                    prompt_tokens=int(usage_data.get("prompt_tokens", 0)),
                                    completion_tokens=int(
                                        usage_data.get("completion_tokens", 0)
                                    ),
                                    total_tokens=int(usage_data.get("total_tokens", 0)),
                                    cost_usd=usage_data.get("cost_usd"),
                                )
                                _accumulate(total_usage, tool_usage)
                                if self.config.user_id is not None:
                                    for alert in self._record_spend_and_maybe_alert(
                                        tool_usage,
                                        model=str(
                                            metadata.get("llm_model") or self.config.model
                                        ),
                                        provider_name=str(
                                            metadata.get("llm_provider")
                                            or getattr(self.provider, "name", "")
                                        ),
                                    ):
                                        yield alert
                                yield AgentEvent.llm_call_complete(
                                    iteration=iteration,
                                    model=str(metadata.get("llm_model") or self.config.model),
                                    usage=vars(tool_usage),
                                    duration_ms=int(metadata.get("llm_duration_ms") or 0),
                                )
                                if (
                                    limits.max_total_tokens is not None
                                    and total_usage.total_tokens >= limits.max_total_tokens
                                ):
                                    tool_limit_reason = "token_limit"
                                elif (
                                    limits.max_cost_usd is not None
                                    and (total_usage.cost_usd or 0.0)
                                    >= limits.max_cost_usd
                                ):
                                    tool_limit_reason = "cost_limit"
                        yield ev
                        if tool_limit_reason is not None:
                            yield AgentEvent.finish(
                                reason=tool_limit_reason,
                                usage=total_usage,
                                iterations=iteration,
                                elapsed_ms=_elapsed_ms(run_started),
                            )
                            return

            # The model spent every allowed iteration calling tools. Instead of
            # stopping silently mid-task (leaving a bare tool result as the last
            # word), give it one final tool-less turn so it summarizes what it
            # accomplished and what remains. The run still finishes with
            # reason="max_iterations" so the UI can flag the ceiling.
            if not self._is_cancelled():
                async for ev in self._max_iterations_summary(total_usage):
                    yield ev

            yield AgentEvent.finish(
                reason="max_iterations",
                usage=total_usage,
                iterations=limits.max_iterations,
                elapsed_ms=_elapsed_ms(run_started),
            )
        finally:
            # Always release the run context, even on early return / exception,
            # so a cancelled turn doesn't leak its workdir into the next one.
            reset_run_context(ctx_token)
            # Drop the run from the cancellation registry once the loop exits.
            # After this the run is no longer cancellable, which is correct.
            if self.config.cancellable and self.config.run_id is not None:
                run_registry.unregister(self.config.run_id)

    # ---- internals ----

    async def _max_iterations_summary(self, total_usage: Usage) -> AsyncIterator[AgentEvent]:
        """One final tool-less LLM turn after the iteration ceiling is hit.

        Nudges the model to wrap up with a short summary instead of ending the
        turn on a bare tool result. The nudge lives only in the in-memory
        history (persistence is event-driven, so it is never stored as a user
        row); the summary itself is emitted as a ``message`` event so the runner
        persists it like any other assistant turn.
        """
        # Respect the cost budget: skip the wrap-up call if it would be blocked.
        if self.config.user_id is not None:
            try:
                with Session(engine) as _budget_session:
                    _eval = budget_evaluation(_budget_session, user_id=self.config.user_id)
                if _eval is not None and _eval.blocked:
                    return
            except Exception as exc:  # a failed check must not block the summary
                log.warning("agent.summary_budget_check_failed", error=str(exc))

        self.history.append(
            Message(
                role="user",
                content=(
                    "[System] You have reached the maximum number of tool-call "
                    "iterations for this turn and can no longer use tools. Do not "
                    "attempt any more tool calls. Briefly summarize what you "
                    "accomplished so far and what remains to be done."
                ),
            )
        )
        parts: list[str] = []
        usage: Usage | None = None
        try:
            async for event in self.provider.chat_completion_stream(
                self.history,
                model=self.config.model,
                tools=None,  # force a text-only wrap-up
                temperature=self.config.temperature,
                max_tokens=self.config.max_tokens,
            ):
                if event.delta:
                    parts.append(event.delta)
                    yield AgentEvent.token(event.delta)
                if event.usage:
                    usage = event.usage
        except Exception as exc:
            log.warning("agent.max_iterations_summary_failed", error=str(exc))
            return
        if usage:
            _accumulate(total_usage, usage)
        content = "".join(parts) or None
        if content:
            self.history.append(Message(role="assistant", content=content))
            yield AgentEvent.message(content=content, tool_calls=None)

    def _is_cancelled(self) -> bool:
        """True if this run has been signalled to cancel (or isn't cancellable-safe).

        Returns False when the run isn't cancellable (run_id unset or
        ``cancellable`` False), so non-interactive runners and tests behave
        exactly as before.
        """
        if not self.config.cancellable or self.config.run_id is None:
            return False
        return run_registry.is_cancelled(self.config.run_id)

    def _record_spend_and_maybe_alert(
        self,
        call_usage: Usage,
        *,
        model: str | None = None,
        provider_name: str | None = None,
    ) -> list[AgentEvent]:
        """Persist this LLM call's spend and emit a budget_alert if warranted.

        Returns a (possibly empty) list of events for the loop to yield. Uses a
        single short-lived session for both spend recording and budget evaluation
        (item 8: eliminates the redundant second session open). Errors are logged
        and swallowed — spend accounting must not break a turn.
        """
        resolved_provider_name = provider_name or getattr(self.provider, "name", "") or ""
        events: list[AgentEvent] = []
        user_id = self.config.user_id
        if user_id is None:
            return events
        try:
            with Session(engine) as session:
                # Record spend (commits internally so evaluation sees it).
                record_spend(
                    session,
                    user_id=user_id,
                    model=model or self.config.model,
                    provider_name=resolved_provider_name,
                    usage=call_usage,
                    run_id=self.config.run_id,
                    conversation_id=self.config.conversation_id,
                )
                # Evaluate budget + fire alerts in the same session.
                evaluation = budget_evaluation(session, user_id=user_id)
                for window, ws in evaluation.windows.items():
                    if not ws.alerted or ws.limit_usd is None:
                        continue
                    # Debounce: fire at most once per period per window.
                    row = session.exec(select(Budget).where(Budget.user_id == user_id)).first()
                    last = getattr(row, "last_alert_at", None) if row else None
                    # Re-fire only if no alert has fired in this window's period.
                    if last is not None and last >= window_start(window):
                        continue
                    events.append(
                        AgentEvent.budget_alert(
                            window=window,
                            spend_usd=ws.spend_usd,
                            limit_usd=ws.limit_usd,
                            pct=ws.pct,
                        )
                    )
                    mark_alert_fired(session, user_id=user_id)
                    break  # one alert event per call is enough
        except Exception as exc:
            log.warning("agent.budget_spend_or_alert_failed", error=str(exc))
        return events

    def _build_run_context(self) -> RunContext:
        """Construct the RunContext (workdir + permissions + capabilities + breakpoints) for this run."""
        from pathlib import Path

        from app.core.config import get_settings

        settings = get_settings()
        if self.config.working_directory:
            workdir = Path(self.config.working_directory)
        elif settings.default_working_directory:
            workdir = Path(settings.default_working_directory)
        else:
            workdir = Path(settings.workspaces_dir)
        perms = self.config.permissions.tools if self.config.permissions else {}
        return RunContext(
            workdir=workdir,
            permissions=dict(perms),
            capability_policy=self.config.capability_policy,
            breakpoints=self.config.breakpoints,
            conversation_id=self.config.conversation_id,
            run_id=self.config.run_id,
            agent_id=self.config.agent_id,
            model=self.config.model,
            skill_names=self.config.skill_names,
        )

    def _resolve_decision(self, name: str, dangerous: bool) -> Decision:
        """Effective allow/ask/deny for a tool, honoring capability + tool policies.

        Two layers are checked and the *more restrictive* wins:
        1. Capability policy (coarse-grained: read/write/execute/network/...)
        2. Per-tool permission map (fine-grained: read_file/python_execute/...)

        Default (no policy configured at all) is ``"allow"``: the MVP is
        single-user and trusted, matching pre-permission behavior. As soon as
        ANY permission or capability config is supplied, that config decides.
        """
        tool = get_tool(name)
        checks = [(name, dangerous)]
        if tool is not None:
            checks.extend(
                (nested_name, bool(nested and nested.dangerous))
                for nested_name in tool.composed_tools
                for nested in [get_tool(nested_name)]
            )

        decision: Decision = "allow"
        for checked_name, checked_dangerous in checks:
            cap_decision: Decision = "allow"
            if self.config.capability_policy is not None:
                cap_decision = self.config.capability_policy.resolve_tool(checked_name)
            tool_decision: Decision = (
                "allow"
                if self.config.permissions is None
                else self.config.permissions.resolve(
                    checked_name, dangerous=checked_dangerous
                )
            )
            decision = stricter(decision, stricter(cap_decision, tool_decision))

        # Non-interactive runners (cron, subagents) treat "ask" as "allow".
        if decision == "ask" and self.config.auto_approve:
            return "allow"
        return decision

    async def _run_tool_call(self, call: dict[str, Any]) -> AsyncIterator[AgentEvent]:
        """Validate, gate, run, and emit events for a single tool call.

        Gates (in order):
        1. ``before_tool`` breakpoint (any tool) — pause before the call.
        2. ``before_write`` breakpoint (write tools only) — pause before write.
        3. Capability + per-tool permission check (allow/ask/deny).
        4. Execute the tool.
        5. ``after_tool_result`` breakpoint — pause after the result.

        Secret masking is applied to the tool result before it's yielded.
        """
        call_id = call.get("id") or call.get("name") or "call"
        name = call.get("name", "")
        args = call.get("arguments") or {}

        yield AgentEvent.tool_call_start(call_id=call_id, name=name, arguments=args)

        # --- Parse error gate: if the stream delivered malformed JSON args,
        # fail explicitly instead of executing with empty/garbage arguments.
        if call.get("_parse_error"):
            result = ToolResult.err(
                f"Failed to parse arguments for tool '{name}' "
                "(stream may have been interrupted). Please retry."
            )
            result.metadata = {"parse_error": True, "duration_ms": 0}
            await self._finalize_tool_call(call_id, name, result)
            yield self._masked_tool_result(call_id, name, result)
            return

        tool = get_tool(name)
        t0 = time.monotonic()

        # --- Breakpoint: before_tool (any tool) ---
        if self._should_break(BreakpointType.BEFORE_TOOL, tool_name=name):
            request = self._breakpoint_event(call_id, name, args, BreakpointType.BEFORE_TOOL)
            yield request
            bp_approved, bp_decision = await self._wait_for_approval(request)
            yield self._approval_resolved_event(request, bp_decision)
            if not bp_approved:
                result = ToolResult.err("Breakpoint denied: the action was rejected or timed out.")
                result.metadata = {
                    "denied": True,
                    "breakpoint": "before_tool",
                    "duration_ms": _elapsed_ms(t0),
                }
                await self._finalize_tool_call(call_id, name, result)
                yield self._masked_tool_result(call_id, name, result)
                return

        # --- Breakpoint: before_write (write tools only) ---
        if is_write_tool(name) and self._should_break(BreakpointType.BEFORE_WRITE, tool_name=name):
            request = self._breakpoint_event(call_id, name, args, BreakpointType.BEFORE_WRITE)
            yield request
            bp_approved, bp_decision = await self._wait_for_approval(request)
            yield self._approval_resolved_event(request, bp_decision)
            if not bp_approved:
                result = ToolResult.err("Breakpoint denied: the write was rejected or timed out.")
                result.metadata = {
                    "denied": True,
                    "breakpoint": "before_write",
                    "duration_ms": _elapsed_ms(t0),
                }
                await self._finalize_tool_call(call_id, name, result)
                yield self._masked_tool_result(call_id, name, result)
                return

        # --- Permission gate: capability + per-tool ---
        decision = self._resolve_decision(name, dangerous=bool(tool and tool.dangerous))

        if decision == "ask":
            # For write tools, include the current file content so the UI can
            # render a diff/preview before the user decides (Фаза 1.5 §2).
            extra: dict[str, Any] = {}
            if is_write_tool(name):
                current = self._read_file_for_preview(args)
                if current is not None:
                    extra["current_content"] = current
            approval_identity = self._approval_identity(call_id)
            request = AgentEvent(
                kind="tool_approval_request",
                payload={
                    "id": call_id,
                    "name": name,
                    "arguments": args,
                    "reason": f"Tool {name!r} requires approval",
                    "requires_decision": True,
                    **approval_identity,
                    "composed_tools": list(tool.composed_tools) if tool else [],
                    **extra,
                },
            )
            yield request
            approved, approval_decision = await self._wait_for_approval(request)
            yield self._approval_resolved_event(request, approval_decision)
            if not approved:
                result = ToolResult.err("Permission denied: the request was rejected or timed out.")
                result.metadata = {"denied": True, "duration_ms": _elapsed_ms(t0)}
                await self._finalize_tool_call(call_id, name, result)
                yield self._masked_tool_result(call_id, name, result)
                return
        elif decision == "deny":
            result = ToolResult.err(f"Permission denied (policy): tool {name!r} is blocked.")
            result.metadata = {"denied": True, "duration_ms": _elapsed_ms(t0)}
            await self._finalize_tool_call(call_id, name, result)
            yield self._masked_tool_result(call_id, name, result)
            return

        if tool is None:
            result = ToolResult.err(f"Unknown tool: {name}")
        else:
            log.info("agent.tool.start", name=name, args=args, decision=decision)
            run_context = get_run_context()
            previous_composed = run_context.approved_composed_tools
            run_context.approved_composed_tools = frozenset(tool.composed_tools)
            try:
                result = await tool.run(args)
            finally:
                run_context.approved_composed_tools = previous_composed
            log.info(
                "agent.tool.done",
                name=name,
                success=not result.is_error,
            )
        # Surface how long the tool took so the UI can show it inline.
        if result.metadata is None:
            result.metadata = {}
        result.metadata["duration_ms"] = _elapsed_ms(t0)

        await self._finalize_tool_call(call_id, name, result)
        yield self._masked_tool_result(call_id, name, result)

        # --- Breakpoint: after_tool_result ---
        if self._should_break(BreakpointType.AFTER_TOOL_RESULT, tool_name=name):
            request = self._breakpoint_event(
                call_id,
                name,
                args,
                BreakpointType.AFTER_TOOL_RESULT,
                extra_context={"result_preview": (result.output or "")[:500]},
            )
            yield request
            _, bp_decision = await self._wait_for_approval(request)
            yield self._approval_resolved_event(request, bp_decision)

    def _should_break(self, bp_type: BreakpointType, *, tool_name: str | None = None) -> bool:
        """True if a breakpoint of ``bp_type`` should fire for ``tool_name``."""
        if self.config.breakpoints is None or self.config.breakpoints.is_empty:
            return False
        # Non-interactive runners skip breakpoints (same as auto_approve for ask).
        if self.config.auto_approve:
            return False
        if self.config.breakpoints.should_break(bp_type, tool_name=tool_name) is not None:
            return True
        tool = get_tool(tool_name or "")
        return bool(
            tool
            and any(
                self.config.breakpoints.should_break(bp_type, tool_name=nested_name)
                is not None
                for nested_name in tool.composed_tools
            )
        )

    def _read_file_for_preview(self, args: dict[str, Any]) -> str | None:
        """Read the current content of a file targeted by a write tool.

        Returns the existing content (for diff/preview in the approval UI),
        or None if the file doesn't exist or can't be read. Caps at 50 KB
        to avoid shipping huge payloads in the event stream.
        """
        rel_path = args.get("path")
        if not rel_path or not isinstance(rel_path, str):
            return None
        try:
            ctx = self._build_run_context()
            full = (ctx.workdir / rel_path).resolve()
            # Confinement: only read within the workspace.
            if not str(full).startswith(str(ctx.workdir.resolve())):
                return None
            if not full.is_file():
                return None
            content = full.read_text(encoding="utf-8", errors="replace")
            # Cap to avoid bloating the event payload.
            if len(content) > 50_000:
                content = content[:50_000] + "\n… (truncated)"
            return content
        except Exception:
            return None

    def _breakpoint_event(
        self,
        call_id: str,
        name: str,
        args: dict[str, Any],
        bp_type: BreakpointType,
        *,
        extra_context: dict[str, Any] | None = None,
    ) -> AgentEvent:
        """Build a breakpoint approval-request event (not yet yielded)."""
        payload: dict[str, Any] = {
            "id": call_id,
            "name": name,
            "arguments": args,
            "reason": f"Breakpoint ({bp_type.value}): review before proceeding.",
            "requires_decision": True,
            "is_breakpoint": True,
            "breakpoint_type": bp_type.value,
            **self._approval_identity(call_id),
        }
        # For write-tool breakpoints, include current file content for diff preview.
        if is_write_tool(name):
            current = self._read_file_for_preview(args)
            if current is not None:
                payload["current_content"] = current
        if extra_context:
            payload.update(extra_context)
        return AgentEvent(kind="tool_approval_request", payload=payload)

    def _approval_identity(self, call_id: str) -> dict[str, Any]:
        scope = {
            "actor_id": self.config.user_id,
            "conversation_id": self.config.conversation_id,
            "run_id": self.config.run_id,
        }
        approval_registry.register(call_id, **scope)
        ticket = approval_registry.ticket(call_id, **scope)
        return {
            "approval_id": ticket.approval_id,
            "revision": ticket.revision,
            "run_id": self.config.run_id,
        }

    @staticmethod
    def _approval_resolved_event(request: AgentEvent, decision: str) -> AgentEvent:
        return AgentEvent.tool_approval_resolved(
            call_id=str(request.payload["id"]),
            approval_id=str(request.payload["approval_id"]),
            revision=int(request.payload["revision"]),
            decision=decision,
        )

    def _masked_tool_result(self, call_id: str, name: str, result: ToolResult) -> AgentEvent:
        """Build a tool_result event with secret masking applied to the output."""
        from app.core.config import get_settings

        masked_output = mask_secrets_in_value(result.output, enabled=get_settings().mask_secrets)
        return AgentEvent.tool_result(
            call_id=call_id,
            name=name,
            result={
                "output": masked_output,
                "is_error": result.is_error,
                "error": result.error,
                "metadata": result.metadata,
            },
        )

    async def _wait_for_approval(self, request: AgentEvent) -> tuple[bool, str]:
        """Block until the client resolves the approval (or timeout auto-denies)."""
        import asyncio

        from app.core.config import get_settings

        call_id = str(request.payload["id"])
        approval_id = str(request.payload["approval_id"])
        revision = int(request.payload["revision"])
        future = approval_registry.future(
            approval_id,
            expected_revision=revision,
            actor_id=self.config.user_id,
            conversation_id=self.config.conversation_id,
            run_id=self.config.run_id,
        )
        # Configurable via Settings (default 30s). The module constant is kept
        # only so existing tests can monkeypatch it to shrink the wait.
        timeout = get_settings().approval_timeout_s or DEFAULT_APPROVAL_TIMEOUT_S
        try:
            approved = await asyncio.wait_for(future, timeout=timeout)
            return approved, "approved" if approved else "denied"
        except TimeoutError:
            # Auto-deny on timeout so a forgotten prompt never hangs the turn.
            approval_registry.cancel(approval_id)
            log.warning("approval.timeout", call_id=call_id, timeout_s=timeout)
            return False, "timed_out"
        finally:
            approval_registry.forget(approval_id)

    async def _finalize_tool_call(self, call_id: str, name: str, result: ToolResult) -> None:
        """Append the tool message to history (kept as a helper for clarity)."""
        self.history.append(
            Message(
                role="tool",
                content=result.output,
                tool_call_id=call_id,
                name=name,
            )
        )


# --- module-level helpers --------------------------------------------------


def _elapsed_ms(started: float) -> int:
    """Whole-millisecond elapsed since ``started`` (a time.monotonic() value)."""
    return int((time.monotonic() - started) * 1000)


def _all_tool_names() -> set[str]:
    from app.tools import get_registry

    return set(get_registry().keys())


def _accumulate(total: Usage, delta: Usage) -> None:
    total.prompt_tokens += delta.prompt_tokens
    total.completion_tokens += delta.completion_tokens
    total.total_tokens += delta.total_tokens
    if delta.cost_usd is not None:
        total.cost_usd = (total.cost_usd or 0.0) + delta.cost_usd


def _merge_tool_call_deltas(
    calls: list[dict[str, Any]], delta: dict[str, Any] | list[dict[str, Any]]
) -> None:
    """Merge streamed tool_call delta(s) (OpenAI-shaped) into the accumulator.

    OpenAI streams ``delta.tool_calls`` as a **list** of partial tool-call
    objects, one per chunk. Some callers (and the test ScriptedProvider) emit a
    single dict instead. We accept both: a dict is wrapped in a list, a list is
    iterated. Each entry is OpenAI-shaped::

        {"index": 0, "id": "...", "type": "function",
         "function": {"name": "...", "arguments": "<json-string fragments>"}}
    """
    deltas = [delta] if isinstance(delta, dict) else delta
    for d in deltas:
        _merge_one_tool_call_delta(calls, d)


def _merge_one_tool_call_delta(calls: list[dict[str, Any]], delta: dict[str, Any]) -> None:
    idx = delta.get("index", 0)
    while len(calls) <= idx:
        # Use a list for arguments to avoid O(n²) string concatenation.
        calls.append({"id": None, "type": "function", "function": {"name": "", "arguments": []}})
    target = calls[idx]
    if delta.get("id"):
        target["id"] = delta["id"]
    if delta.get("type"):
        target["type"] = delta["type"]
    fn_delta = delta.get("function") or {}
    fn = target["function"]
    if fn_delta.get("name"):
        fn["name"] += fn_delta["name"]
    if fn_delta.get("arguments"):
        fn["arguments"].append(fn_delta["arguments"])


def _normalize_tool_call(call: dict[str, Any]) -> dict[str, Any]:
    """Parse JSON-string arguments into a dict. Returns a flat call shape.

    If the arguments JSON is malformed (e.g. stream interrupted mid-JSON),
    sets ``_parse_error: True`` in the returned dict so the caller can emit
    an explicit error instead of silently executing with empty args.
    """
    fn = call.get("function") or {}
    name = fn.get("name", "")
    raw_args = fn.get("arguments", "")
    # Join accumulated fragments (list) into a single string.
    if isinstance(raw_args, list):
        raw_args = "".join(raw_args)
    parse_error = False
    if isinstance(raw_args, str):
        try:
            args = json.loads(raw_args) if raw_args.strip() else {}
        except json.JSONDecodeError:
            log.warning(
                "agent.tool_args_parse_error",
                tool_name=name,
                raw_args_preview=raw_args[:200],
            )
            args = {}
            parse_error = True
    else:
        args = raw_args or {}
    result: dict[str, Any] = {
        "id": call.get("id"),
        "type": call.get("type", "function"),
        "name": name,
        "arguments": args,
    }
    if parse_error:
        result["_parse_error"] = True
    return result
