"""EvalScenario: declarative definition of an agent evaluation scenario.

A scenario describes:
- What the user says (input messages)
- What the scripted LLM responds (tool calls or text)
- What we expect the agent to do (assertions on tools called, events emitted, etc.)
- Metadata: tags, severity, description

Scenarios are deterministic by design: they use a ScriptedProvider (or a trace
replay provider) so results are reproducible without real LLM calls.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from enum import Enum
from typing import Any


class Severity(str, Enum):
    """How critical a scenario is for the CI gate.

    CRITICAL: must pass or the gate fails (blocks merge).
    IMPORTANT: regression is reported but doesn't block.
    INFO: informational only (latency/cost tracking).
    """

    CRITICAL = "critical"
    IMPORTANT = "important"
    INFO = "info"


@dataclass
class ScenarioAssertion:
    """A single assertion about the agent's behavior during a scenario.

    Types:
        tool_called:       tool ``name`` was invoked (optionally with matching args)
        tool_not_called:   tool ``name`` was NOT invoked
        tool_order:        tools were called in the given order
        event_emitted:     an event of ``kind`` was emitted
        event_not_emitted: no event of ``kind`` was emitted
        finish_reason:     the run finished with the given reason
        result_contains:   a tool result contains the given substring
        result_is_error:   a tool result for ``name`` is an error
        denied:            a tool call was denied (permission/capability gate)
        max_iterations:    the run stopped at max_iterations
        token_budget:      total tokens stayed under a threshold
    """

    type: str
    # Parameters depend on type:
    name: str | None = None  # tool name or event kind
    arguments: dict[str, Any] | None = None  # expected tool args (subset match)
    order: list[str] | None = None  # for tool_order
    substring: str | None = None  # for result_contains
    reason: str | None = None  # for finish_reason
    max_tokens: int | None = None  # for token_budget

    def describe(self) -> str:
        """Human-readable description for reports."""
        match self.type:
            case "tool_called":
                args_str = f" with args matching {self.arguments}" if self.arguments else ""
                return f"tool '{self.name}' was called{args_str}"
            case "tool_not_called":
                return f"tool '{self.name}' was NOT called"
            case "tool_order":
                return f"tools called in order: {self.order}"
            case "event_emitted":
                return f"event '{self.name}' was emitted"
            case "event_not_emitted":
                return f"event '{self.name}' was NOT emitted"
            case "finish_reason":
                return f"finish reason is '{self.reason}'"
            case "result_contains":
                return f"result of '{self.name}' contains '{self.substring}'"
            case "result_is_error":
                return f"result of '{self.name}' is an error"
            case "denied":
                return f"tool '{self.name}' was denied"
            case "max_iterations":
                return "run stopped at max_iterations"
            case "token_budget":
                return f"total tokens <= {self.max_tokens}"
            case _:
                return f"unknown assertion type: {self.type}"


@dataclass
class EvalScenario:
    """A complete evaluation scenario.

    Attributes:
        id:          unique identifier (e.g. "tool_select_read_file")
        name:        human-readable name
        description: what this scenario tests
        tags:        categorization tags (e.g. ["tool_selection", "files"])
        severity:    how critical this is for CI gates
        input:       the user message(s) that start the scenario
        script:      list of scripted LLM turns (see ScriptedProvider format):
                     - str: plain text response
                     - list[dict]: tool calls [{"name": ..., "arguments": {...}}]
                     - dict: {"text": ..., "reasoning": ..., "tool_calls": [...]}
        assertions:  list of assertions to check after the run
        config:      optional AgentConfig overrides (model, limits, permissions, etc.)
        history:     optional pre-existing conversation history
        timeout_s:   max wall-clock time for the scenario (default 30s)
    """

    id: str
    name: str
    description: str = ""
    tags: list[str] = field(default_factory=list)
    severity: Severity = Severity.IMPORTANT
    input: str = ""
    script: list[Any] = field(default_factory=list)
    assertions: list[ScenarioAssertion] = field(default_factory=list)
    config: dict[str, Any] = field(default_factory=dict)
    history: list[dict[str, Any]] = field(default_factory=list)
    timeout_s: float = 30.0

    def __repr__(self) -> str:
        return f"<EvalScenario id={self.id!r} severity={self.severity.value} tags={self.tags}>"


@dataclass
class AssertionResult:
    """Result of a single assertion check."""

    assertion: ScenarioAssertion
    passed: bool
    detail: str = ""  # explanation on failure


@dataclass
class ScenarioResult:
    """Result of running a single scenario.

    Captures pass/fail, timing, token usage, and per-assertion outcomes.
    Serializable to JSON for baseline storage and comparison.
    """

    scenario_id: str
    scenario_name: str
    passed: bool
    severity: str
    tags: list[str]
    assertion_results: list[AssertionResult] = field(default_factory=list)
    # Metrics
    elapsed_ms: float = 0.0
    total_tokens: int = 0
    iterations: int = 0
    finish_reason: str = ""
    tools_called: list[str] = field(default_factory=list)
    events: list[dict[str, Any]] = field(default_factory=list)
    error: str | None = None

    @property
    def failed_assertions(self) -> list[AssertionResult]:
        return [a for a in self.assertion_results if not a.passed]

    def to_dict(self) -> dict[str, Any]:
        """Serialize to a JSON-compatible dict (for baseline storage)."""
        return {
            "scenario_id": self.scenario_id,
            "scenario_name": self.scenario_name,
            "passed": self.passed,
            "severity": self.severity,
            "tags": self.tags,
            "elapsed_ms": round(self.elapsed_ms, 2),
            "total_tokens": self.total_tokens,
            "iterations": self.iterations,
            "finish_reason": self.finish_reason,
            "tools_called": self.tools_called,
            "assertion_results": [
                {
                    "type": ar.assertion.type,
                    "name": ar.assertion.name,
                    "passed": ar.passed,
                    "detail": ar.detail,
                }
                for ar in self.assertion_results
            ],
            "error": self.error,
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> ScenarioResult:
        """Deserialize from a baseline dict."""
        assertion_results = [
            AssertionResult(
                assertion=ScenarioAssertion(type=ar["type"], name=ar.get("name")),
                passed=ar["passed"],
                detail=ar.get("detail", ""),
            )
            for ar in data.get("assertion_results", [])
        ]
        return cls(
            scenario_id=data["scenario_id"],
            scenario_name=data["scenario_name"],
            passed=data["passed"],
            severity=data["severity"],
            tags=data.get("tags", []),
            assertion_results=assertion_results,
            elapsed_ms=data.get("elapsed_ms", 0.0),
            total_tokens=data.get("total_tokens", 0),
            iterations=data.get("iterations", 0),
            finish_reason=data.get("finish_reason", ""),
            tools_called=data.get("tools_called", []),
            error=data.get("error"),
        )
