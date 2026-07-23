"""Agent evals: scenario-driven quality gates for the agent loop.

This package provides:
- EvalScenario: declarative test-case definition (input, expected behavior, assertions)
- EvalRunner: executes scenarios against the AgentExecutor, collects metrics
- Built-in scenario suites: tool selection, safety refusal, cost limits
- Replay & comparison: replay saved traces with different model/prompt, compare results
- CI gate: compare results against a baseline, fail on regression

Usage (CLI):
    python -m evals                     # run all scenarios
    python -m evals --tag safety        # run only safety scenarios
    python -m evals --compare baseline  # compare against saved baseline
    python -m evals --update-baseline   # save current results as new baseline
"""

from evals.scenario import EvalScenario, ScenarioAssertion, ScenarioResult
from evals.runner import EvalRunner

__all__ = [
    "EvalRunner",
    "EvalScenario",
    "ScenarioAssertion",
    "ScenarioResult",
]
