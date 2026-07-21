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
from collections.abc import AsyncIterator
from dataclasses import dataclass, field
from typing import Any

from app.agent.events import AgentEvent
from app.core.logging import get_logger
from app.providers import (
    LLMProvider,
    Message,
    ToolSpec,
    Usage,
)
from app.tools import ToolResult, get_tool

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
        yield AgentEvent.start()

        for iteration in range(1, limits.max_iterations + 1):
            content_parts: list[str] = []
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
                    if event.delta:
                        content_parts.append(event.delta)
                        yield AgentEvent.token(event.delta)
                    if event.tool_call_delta:
                        _merge_tool_call_delta(tool_calls, event.tool_call_delta)
                    if event.usage:
                        usage = event.usage
                    if event.finish:
                        finish_reason = event.finish_reason or "stop"
            except Exception as exc:
                log.error("agent.iteration_failed", iteration=iteration, error=str(exc))
                yield AgentEvent.error(
                    f"LLM error on iteration {iteration}", detail=str(exc)
                )
                return

            if usage:
                _accumulate(total_usage, usage)

            content = "".join(content_parts) or None
            # Normalize tool_calls: parse JSON-string arguments into dicts.
            normalized_calls = [_normalize_tool_call(c) for c in tool_calls] if tool_calls else None

            self.history.append(
                Message(
                    role="assistant",
                    content=content,
                    tool_calls=normalized_calls,
                )
            )
            yield AgentEvent.message(content=content, tool_calls=normalized_calls)

            # Enforce ceilings.
            if (
                limits.max_total_tokens is not None
                and total_usage.total_tokens >= limits.max_total_tokens
            ):
                yield AgentEvent.finish(
                    reason="token_limit",
                    usage=total_usage,
                    iterations=iteration,
                )
                return

            if not normalized_calls:
                yield AgentEvent.finish(
                    reason=finish_reason or "stop",
                    usage=total_usage,
                    iterations=iteration,
                )
                return

            # Execute tool calls, append results, continue the loop.
            for call in normalized_calls:
                async for ev in self._run_tool_call(call):
                    yield ev

        yield AgentEvent.finish(
            reason="max_iterations",
            usage=total_usage,
            iterations=limits.max_iterations,
        )

    # ---- internals ----

    async def _run_tool_call(self, call: dict[str, Any]) -> AsyncIterator[AgentEvent]:
        """Validate, run, and emit events for a single tool call."""
        call_id = call.get("id") or call.get("name") or "call"
        name = call.get("name", "")
        args = call.get("arguments") or {}

        yield AgentEvent.tool_call_start(call_id=call_id, name=name, arguments=args)

        tool = get_tool(name)
        if tool is None:
            result = ToolResult.err(f"Unknown tool: {name}")
        else:
            log.info("agent.tool.start", name=name, args=args)
            result = await tool.run(args)
            log.info(
                "agent.tool.done",
                name=name,
                success=not result.is_error,
            )

        self.history.append(
            Message(
                role="tool",
                content=result.output,
                tool_call_id=call_id,
                name=name,
            )
        )

        yield AgentEvent.tool_result(
            call_id=call_id,
            name=name,
            result={
                "output": result.output,
                "is_error": result.is_error,
                "error": result.error,
                "metadata": result.metadata,
            },
        )


# --- module-level helpers --------------------------------------------------


def _all_tool_names() -> set[str]:
    from app.tools import get_registry

    return set(get_registry().keys())


def _accumulate(total: Usage, delta: Usage) -> None:
    total.prompt_tokens += delta.prompt_tokens
    total.completion_tokens += delta.completion_tokens
    total.total_tokens += delta.total_tokens
    if delta.cost_usd is not None:
        total.cost_usd = (total.cost_usd or 0.0) + delta.cost_usd


def _merge_tool_call_delta(calls: list[dict[str, Any]], delta: dict[str, Any]) -> None:
    """Merge a streamed tool_call delta (OpenAI-shaped) into the accumulator."""
    idx = delta.get("index", 0)
    while len(calls) <= idx:
        calls.append(
            {"id": None, "type": "function", "function": {"name": "", "arguments": ""}}
        )
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
