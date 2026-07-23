"""EvalRunner: executes scenarios against the AgentExecutor and checks assertions.

The runner:
1. Constructs a ScriptedProvider with the scenario's script
2. Builds an AgentConfig from the scenario's config overrides
3. Runs the agent loop, collecting all events
4. Evaluates assertions against the collected events
5. Returns a ScenarioResult with pass/fail and metrics

Supports both deterministic (scripted) and live (real LLM) execution modes.
"""

from __future__ import annotations

import time
from collections.abc import AsyncIterator
from typing import Any

from app.agent.events import AgentEvent
from app.agent.executor import AgentConfig, AgentExecutor, AgentLimits
from app.agent.permissions import PermissionsConfig
from app.providers import ChatStreamEvent, LLMProvider, Message, ToolSpec, Usage
from app.security.capabilities import CapabilityPolicy

from evals.scenario import (
    AssertionResult,
    EvalScenario,
    ScenarioAssertion,
    ScenarioResult,
)


class ScriptedEvalProvider(LLMProvider):
    """A provider that replays a scripted sequence of turns.

    Identical to the test ScriptedProvider but lives in the evals package
    so evals can run independently of the test suite.
    """

    name = "eval-scripted"

    def __init__(self, *, default_model: str = "eval-model") -> None:
        self.default_model = default_model
        self.turns: list[Any] = []
        self.calls: list[list[Message]] = []

    def set_script(self, turns: list[Any]) -> None:
        self.turns = list(turns)

    async def chat_completion(self, messages, *, model, tools=None, **kwargs):  # type: ignore[override]
        raise NotImplementedError("ScriptedEvalProvider only implements streaming")

    async def chat_completion_stream(  # type: ignore[override]
        self,
        messages: list[Message],
        *,
        model: str,
        tools: list[ToolSpec] | None = None,
        temperature: float = 0.7,
        max_tokens: int | None = None,
        **kwargs: Any,
    ) -> AsyncIterator[ChatStreamEvent]:
        self.calls.append(list(messages))
        if not self.turns:
            raise RuntimeError("ScriptedEvalProvider: script exhausted")
        turn = self.turns.pop(0)

        text: str = ""
        reasoning: str = ""
        tool_calls: list[dict[str, Any]] | None = None
        if isinstance(turn, str):
            text = turn
        elif isinstance(turn, list):
            tool_calls = turn
        elif isinstance(turn, dict):
            text = turn.get("text", "")
            reasoning = turn.get("reasoning", "")
            tool_calls = turn.get("tool_calls")
        else:
            raise TypeError(f"Bad scripted turn: {turn!r}")

        if reasoning:
            for word in reasoning.split(" "):
                yield ChatStreamEvent(reasoning=word + " ")
        if text:
            for word in text.split(" "):
                yield ChatStreamEvent(delta=word + " ")
        if tool_calls:
            import json as _json

            for idx, call in enumerate(tool_calls):
                args = call.get("arguments", {})
                yield ChatStreamEvent(
                    tool_call_delta={
                        "index": idx,
                        "id": call.get("id", f"call_{idx}"),
                        "type": "function",
                        "function": {
                            "name": call["name"],
                            "arguments": _json.dumps(args),
                        },
                    }
                )
        yield ChatStreamEvent(
            finish=True,
            usage=Usage(prompt_tokens=10, completion_tokens=5, total_tokens=15),
        )


class EvalRunner:
    """Executes eval scenarios and produces results.

    Usage:
        runner = EvalRunner()
        results = await runner.run_all(scenarios)
        # or
        result = await runner.run_one(scenario)
    """

    def __init__(self, *, provider: LLMProvider | None = None) -> None:
        self._provider = provider

    async def run_all(
        self,
        scenarios: list[EvalScenario],
        *,
        tags: list[str] | None = None,
        concurrency: int = 1,
    ) -> list[ScenarioResult]:
        """Run multiple scenarios, optionally filtered by tags.

        Args:
            scenarios: list of scenarios to run
            tags: if provided, only run scenarios that have at least one of these tags
            concurrency: reserved for future parallel execution (currently sequential)
        """
        filtered = scenarios
        if tags:
            tag_set = set(tags)
            filtered = [s for s in scenarios if tag_set & set(s.tags)]

        results: list[ScenarioResult] = []
        for scenario in filtered:
            result = await self.run_one(scenario)
            results.append(result)
        return results

    async def run_one(self, scenario: EvalScenario) -> ScenarioResult:
        """Execute a single scenario and evaluate its assertions."""
        provider = self._provider or ScriptedEvalProvider()
        if isinstance(provider, ScriptedEvalProvider):
            provider.set_script(scenario.script)

        config = self._build_config(scenario)
        history = self._build_history(scenario)

        executor = AgentExecutor(
            provider=provider,
            config=config,
            history=history,
        )

        events: list[AgentEvent] = []
        t0 = time.monotonic()
        error: str | None = None

        try:
            async for event in executor.stream(scenario.input or None):
                events.append(event)
        except Exception as exc:
            error = f"{type(exc).__name__}: {exc}"

        elapsed_ms = (time.monotonic() - t0) * 1000

        # Extract metrics from events
        tools_called = _extract_tools_called(events)
        finish_reason = _extract_finish_reason(events)
        total_tokens = _extract_total_tokens(events)
        iterations = _extract_iterations(events)

        # Evaluate assertions
        assertion_results = [
            self._check_assertion(a, events, tools_called, finish_reason, total_tokens)
            for a in scenario.assertions
        ]

        all_passed = all(ar.passed for ar in assertion_results) and error is None

        return ScenarioResult(
            scenario_id=scenario.id,
            scenario_name=scenario.name,
            passed=all_passed,
            severity=scenario.severity.value,
            tags=scenario.tags,
            assertion_results=assertion_results,
            elapsed_ms=elapsed_ms,
            total_tokens=total_tokens,
            iterations=iterations,
            finish_reason=finish_reason,
            tools_called=tools_called,
            events=[e.to_dict() for e in events],
            error=error,
        )

    def _build_config(self, scenario: EvalScenario) -> AgentConfig:
        """Build an AgentConfig from scenario overrides."""
        cfg = scenario.config
        limits_cfg = cfg.get("limits", {})
        limits = AgentLimits(
            max_iterations=limits_cfg.get("max_iterations", 10),
            max_total_tokens=limits_cfg.get("max_total_tokens"),
            max_cost_usd=limits_cfg.get("max_cost_usd"),
        )

        permissions = None
        if "permissions" in cfg:
            permissions = PermissionsConfig(tools=cfg["permissions"])

        capability_policy = None
        if "capability_policy" in cfg:
            capability_policy = CapabilityPolicy(caps=cfg["capability_policy"])

        return AgentConfig(
            model=cfg.get("model", "eval-model"),
            system_prompt=cfg.get("system_prompt"),
            temperature=cfg.get("temperature", 0.7),
            max_tokens=cfg.get("max_tokens"),
            tool_names=cfg.get("tool_names"),
            limits=limits,
            working_directory=cfg.get("working_directory"),
            permissions=permissions,
            capability_policy=capability_policy,
            auto_approve=cfg.get("auto_approve", True),
        )

    def _build_history(self, scenario: EvalScenario) -> list[Message]:
        """Build initial history from scenario definition."""
        return [
            Message(
                role=m.get("role", "user"),
                content=m.get("content"),
                tool_calls=m.get("tool_calls"),
                tool_call_id=m.get("tool_call_id"),
                name=m.get("name"),
            )
            for m in scenario.history
        ]

    def _check_assertion(
        self,
        assertion: ScenarioAssertion,
        events: list[AgentEvent],
        tools_called: list[str],
        finish_reason: str,
        total_tokens: int,
    ) -> AssertionResult:
        """Evaluate a single assertion against collected events."""
        match assertion.type:
            case "tool_called":
                return self._assert_tool_called(assertion, events, tools_called)
            case "tool_not_called":
                passed = assertion.name not in tools_called
                detail = "" if passed else f"'{assertion.name}' was called"
                return AssertionResult(assertion=assertion, passed=passed, detail=detail)
            case "tool_order":
                return self._assert_tool_order(assertion, tools_called)
            case "event_emitted":
                passed = any(e.kind == assertion.name for e in events)
                detail = "" if passed else f"no event of kind '{assertion.name}' found"
                return AssertionResult(assertion=assertion, passed=passed, detail=detail)
            case "event_not_emitted":
                passed = not any(e.kind == assertion.name for e in events)
                detail = "" if passed else f"event '{assertion.name}' was emitted"
                return AssertionResult(assertion=assertion, passed=passed, detail=detail)
            case "finish_reason":
                passed = finish_reason == assertion.reason
                detail = "" if passed else f"expected '{assertion.reason}', got '{finish_reason}'"
                return AssertionResult(assertion=assertion, passed=passed, detail=detail)
            case "result_contains":
                return self._assert_result_contains(assertion, events)
            case "result_is_error":
                return self._assert_result_is_error(assertion, events)
            case "denied":
                return self._assert_denied(assertion, events)
            case "max_iterations":
                passed = finish_reason == "max_iterations"
                detail = "" if passed else f"finish_reason is '{finish_reason}', not 'max_iterations'"
                return AssertionResult(assertion=assertion, passed=passed, detail=detail)
            case "token_budget":
                budget = assertion.max_tokens or 0
                passed = total_tokens <= budget
                detail = "" if passed else f"tokens={total_tokens} exceeds budget={budget}"
                return AssertionResult(assertion=assertion, passed=passed, detail=detail)
            case _:
                return AssertionResult(
                    assertion=assertion,
                    passed=False,
                    detail=f"unknown assertion type: {assertion.type}",
                )

    def _assert_tool_called(
        self, assertion: ScenarioAssertion, events: list[AgentEvent], tools_called: list[str]
    ) -> AssertionResult:
        if assertion.name not in tools_called:
            return AssertionResult(
                assertion=assertion,
                passed=False,
                detail=f"'{assertion.name}' not in called tools: {tools_called}",
            )
        # If arguments specified, check they match
        if assertion.arguments:
            for e in events:
                if e.kind == "tool_call_start" and e.payload.get("name") == assertion.name:
                    actual_args = e.payload.get("arguments", {})
                    if _args_match(assertion.arguments, actual_args):
                        return AssertionResult(assertion=assertion, passed=True)
            return AssertionResult(
                assertion=assertion,
                passed=False,
                detail=f"'{assertion.name}' called but args don't match {assertion.arguments}",
            )
        return AssertionResult(assertion=assertion, passed=True)

    def _assert_tool_order(
        self, assertion: ScenarioAssertion, tools_called: list[str]
    ) -> AssertionResult:
        expected = assertion.order or []
        # Check that expected tools appear in order (not necessarily consecutive)
        idx = 0
        for tool in tools_called:
            if idx < len(expected) and tool == expected[idx]:
                idx += 1
        passed = idx == len(expected)
        detail = "" if passed else f"expected order {expected}, got {tools_called}"
        return AssertionResult(assertion=assertion, passed=passed, detail=detail)

    def _assert_result_contains(
        self, assertion: ScenarioAssertion, events: list[AgentEvent]
    ) -> AssertionResult:
        for e in events:
            if e.kind == "tool_result" and e.payload.get("name") == assertion.name:
                result = e.payload.get("result", {})
                output = result.get("output", "")
                if assertion.substring and assertion.substring in output:
                    return AssertionResult(assertion=assertion, passed=True)
        return AssertionResult(
            assertion=assertion,
            passed=False,
            detail=f"no result of '{assertion.name}' contains '{assertion.substring}'",
        )

    def _assert_result_is_error(
        self, assertion: ScenarioAssertion, events: list[AgentEvent]
    ) -> AssertionResult:
        for e in events:
            if e.kind == "tool_result" and e.payload.get("name") == assertion.name:
                result = e.payload.get("result", {})
                if result.get("is_error"):
                    return AssertionResult(assertion=assertion, passed=True)
        return AssertionResult(
            assertion=assertion,
            passed=False,
            detail=f"no error result found for '{assertion.name}'",
        )

    def _assert_denied(
        self, assertion: ScenarioAssertion, events: list[AgentEvent]
    ) -> AssertionResult:
        for e in events:
            if e.kind == "tool_result" and e.payload.get("name") == assertion.name:
                result = e.payload.get("result", {})
                metadata = result.get("metadata") or {}
                if metadata.get("denied"):
                    return AssertionResult(assertion=assertion, passed=True)
        return AssertionResult(
            assertion=assertion,
            passed=False,
            detail=f"tool '{assertion.name}' was not denied",
        )


# --- helpers ---


def _extract_tools_called(events: list[AgentEvent]) -> list[str]:
    """Extract ordered list of tool names from tool_call_start events."""
    return [
        e.payload["name"]
        for e in events
        if e.kind == "tool_call_start" and "name" in e.payload
    ]


def _extract_finish_reason(events: list[AgentEvent]) -> str:
    """Extract the finish reason from the finish event."""
    for e in reversed(events):
        if e.kind == "finish":
            return e.payload.get("reason", "")
    return ""


def _extract_total_tokens(events: list[AgentEvent]) -> int:
    """Extract total token count from the finish event's usage."""
    for e in reversed(events):
        if e.kind == "finish":
            usage = e.payload.get("usage")
            if usage:
                return usage.get("total_tokens", 0)
    return 0


def _extract_iterations(events: list[AgentEvent]) -> int:
    """Extract iteration count from the finish event."""
    for e in reversed(events):
        if e.kind == "finish":
            return e.payload.get("iterations", 0)
    return 0


def _args_match(expected: dict[str, Any], actual: dict[str, Any]) -> bool:
    """Check that expected args are a subset of actual args (shallow)."""
    for key, value in expected.items():
        if key not in actual:
            return False
        if actual[key] != value:
            return False
    return True
