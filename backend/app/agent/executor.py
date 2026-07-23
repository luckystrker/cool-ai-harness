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

from app.agent.approvals import DEFAULT_APPROVAL_TIMEOUT_S, approval_registry
from app.agent.events import AgentEvent
from app.agent.permissions import Decision, PermissionsConfig
from app.agent.runs import run_registry
from app.core.logging import get_logger
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
from app.tools.context import RunContext, reset_run_context, set_run_context

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
        names = self.config.tool_names or list(_all_tool_names())
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

                content_parts: list[str] = []
                reasoning_parts: list[str] = []
                tool_calls: list[dict[str, Any]] = []
                usage: Usage | None = None
                finish_reason: str | None = None

                try:
                    async for event in self.provider.chat_completion_stream(
                        self.history,
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

                # Cost budget. cost_usd stays None until providers compute it
                # (Фаза 1.5 §5); until then this guard is inert.
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

                # --- ReAct: emit Thought phase ---
                react_step += 1
                thought_text = thinking or content or f"Deciding to use tool(s) to proceed."
                yield AgentEvent.react_thought(step=react_step, text=thought_text)

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

                for call in normalized_calls:
                    # --- ReAct: emit Action phase ---
                    yield AgentEvent.react_action(
                        step=react_step,
                        tool_name=call.get("name", ""),
                        arguments=call.get("arguments") or {},
                        call_id=call.get("id") or call.get("name") or "call",
                    )
                    async for ev in self._run_tool_call(call):
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

    def _is_cancelled(self) -> bool:
        """True if this run has been signalled to cancel (or isn't cancellable-safe).

        Returns False when the run isn't cancellable (run_id unset or
        ``cancellable`` False), so non-interactive runners and tests behave
        exactly as before.
        """
        if not self.config.cancellable or self.config.run_id is None:
            return False
        return run_registry.is_cancelled(self.config.run_id)

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
        # Layer 1: capability policy (coarse-grained).
        cap_decision: Decision = "allow"
        if self.config.capability_policy is not None:
            cap_decision = self.config.capability_policy.resolve_tool(name)

        # Layer 2: per-tool permission map (fine-grained).
        if self.config.permissions is None:
            tool_decision: Decision = "allow"
        else:
            tool_decision = self.config.permissions.resolve(name, dangerous=dangerous)

        # The more restrictive of the two layers wins.
        decision = stricter(cap_decision, tool_decision)

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

        tool = get_tool(name)
        t0 = time.monotonic()

        # --- Breakpoint: before_tool (any tool) ---
        if self._should_break(BreakpointType.BEFORE_TOOL, tool_name=name):
            yield self._breakpoint_event(call_id, name, args, BreakpointType.BEFORE_TOOL)
            bp_approved = await self._wait_for_approval(call_id)
            if not bp_approved:
                result = ToolResult.err("Breakpoint denied: the action was rejected or timed out.")
                result.metadata = {"denied": True, "breakpoint": "before_tool", "duration_ms": _elapsed_ms(t0)}
                await self._finalize_tool_call(call_id, name, result)
                yield self._masked_tool_result(call_id, name, result)
                return

        # --- Breakpoint: before_write (write tools only) ---
        if is_write_tool(name) and self._should_break(BreakpointType.BEFORE_WRITE, tool_name=name):
            yield self._breakpoint_event(call_id, name, args, BreakpointType.BEFORE_WRITE)
            bp_approved = await self._wait_for_approval(call_id)
            if not bp_approved:
                result = ToolResult.err("Breakpoint denied: the write was rejected or timed out.")
                result.metadata = {"denied": True, "breakpoint": "before_write", "duration_ms": _elapsed_ms(t0)}
                await self._finalize_tool_call(call_id, name, result)
                yield self._masked_tool_result(call_id, name, result)
                return

        # --- Permission gate: capability + per-tool ---
        decision = self._resolve_decision(name, dangerous=bool(tool and tool.dangerous))

        if decision == "ask":
            yield AgentEvent.tool_approval_request(
                call_id=call_id,
                name=name,
                arguments=args,
                reason=f"Tool {name!r} requires approval",
            )
            approved = await self._wait_for_approval(call_id)
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
            result = await tool.run(args)
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
            yield self._breakpoint_event(
                call_id, name, args, BreakpointType.AFTER_TOOL_RESULT,
                extra_context={"result_preview": (result.output or "")[:500]},
            )
            await self._wait_for_approval(call_id)

    def _should_break(self, bp_type: BreakpointType, *, tool_name: str | None = None) -> bool:
        """True if a breakpoint of ``bp_type`` should fire for ``tool_name``."""
        if self.config.breakpoints is None or self.config.breakpoints.is_empty:
            return False
        # Non-interactive runners skip breakpoints (same as auto_approve for ask).
        if self.config.auto_approve:
            return False
        return self.config.breakpoints.should_break(bp_type, tool_name=tool_name) is not None

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
        }
        if extra_context:
            payload.update(extra_context)
        return AgentEvent(kind="tool_approval_request", payload=payload)

    def _masked_tool_result(self, call_id: str, name: str, result: ToolResult) -> AgentEvent:
        """Build a tool_result event with secret masking applied to the output."""
        from app.core.config import get_settings

        masked_output = mask_secrets_in_value(
            result.output, enabled=get_settings().mask_secrets
        )
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

    async def _wait_for_approval(self, call_id: str) -> bool:
        """Block until the client resolves the approval (or timeout auto-denies)."""
        import asyncio

        from app.core.config import get_settings

        future = approval_registry.register(call_id)
        # Configurable via Settings (default 30s). The module constant is kept
        # only so existing tests can monkeypatch it to shrink the wait.
        timeout = get_settings().approval_timeout_s or DEFAULT_APPROVAL_TIMEOUT_S
        try:
            return await asyncio.wait_for(future, timeout=timeout)
        except TimeoutError:
            # Auto-deny on timeout so a forgotten prompt never hangs the turn.
            approval_registry.cancel(call_id)
            log.warning("approval.timeout", call_id=call_id, timeout_s=timeout)
            return False

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
        calls.append({"id": None, "type": "function", "function": {"name": "", "arguments": ""}})
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
        fn["arguments"] += fn_delta["arguments"]


def _normalize_tool_call(call: dict[str, Any]) -> dict[str, Any]:
    """Parse JSON-string arguments into a dict. Returns a flat call shape."""
    fn = call.get("function") or {}
    name = fn.get("name", "")
    raw_args = fn.get("arguments", "")
    if isinstance(raw_args, str):
        try:
            args = json.loads(raw_args) if raw_args.strip() else {}
        except json.JSONDecodeError:
            args = {}
    else:
        args = raw_args or {}
    return {
        "id": call.get("id"),
        "type": call.get("type", "function"),
        "name": name,
        "arguments": args,
    }
