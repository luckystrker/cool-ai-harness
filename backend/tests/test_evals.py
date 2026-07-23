"""Tests for the agent evals framework.

Verifies:
- EvalRunner correctly executes scenarios and checks assertions
- Built-in scenarios pass with the current agent implementation
- Replay/comparison logic works correctly
- CI gate logic (pass/fail on critical regressions)
"""

from __future__ import annotations

import pytest

from evals.replay import ComparisonReport, TraceStore, compare_runs
from evals.runner import EvalRunner, ScriptedEvalProvider
from evals.scenario import (
    AssertionResult,
    EvalScenario,
    ScenarioAssertion,
    ScenarioResult,
    Severity,
)
from evals.scenarios import ALL_SCENARIOS, COST_LIMIT_SCENARIOS, SAFETY_SCENARIOS, TOOL_SELECTION_SCENARIOS


# --- EvalRunner unit tests ---


class TestEvalRunner:
    """Unit tests for the EvalRunner."""

    async def test_simple_text_response(self):
        """Agent responds with text, no tools called."""
        scenario = EvalScenario(
            id="test_text",
            name="Text response",
            input="Hello",
            script=["Hi there!"],
            assertions=[
                ScenarioAssertion(type="finish_reason", reason="stop"),
                ScenarioAssertion(type="tool_not_called", name="read_file"),
            ],
        )
        runner = EvalRunner()
        result = await runner.run_one(scenario)
        assert result.passed
        assert result.finish_reason == "stop"
        assert result.tools_called == []

    async def test_tool_call_scenario(self):
        """Agent calls a tool and the assertion verifies it."""
        scenario = EvalScenario(
            id="test_tool",
            name="Tool call",
            input="Read a file",
            script=[
                [{"name": "read_file", "arguments": {"path": "test.txt"}}],
                "File contents here.",
            ],
            assertions=[
                ScenarioAssertion(type="tool_called", name="read_file"),
                ScenarioAssertion(type="finish_reason", reason="stop"),
            ],
        )
        runner = EvalRunner()
        result = await runner.run_one(scenario)
        assert result.passed
        assert "read_file" in result.tools_called

    async def test_tool_order_assertion(self):
        """Verify tool_order assertion works."""
        scenario = EvalScenario(
            id="test_order",
            name="Tool order",
            input="Do things",
            script=[
                [{"name": "list_files", "arguments": {"path": "."}}],
                [{"name": "read_file", "arguments": {"path": "a.txt"}}],
                "Done.",
            ],
            assertions=[
                ScenarioAssertion(type="tool_order", order=["list_files", "read_file"]),
            ],
        )
        runner = EvalRunner()
        result = await runner.run_one(scenario)
        assert result.passed

    async def test_denied_assertion(self):
        """Verify denied assertion when capability policy blocks a tool."""
        scenario = EvalScenario(
            id="test_denied",
            name="Denied tool",
            input="Execute code",
            script=[
                [{"name": "python_execute", "arguments": {"code": "print(1)"}}],
                "Cannot do that.",
            ],
            assertions=[
                ScenarioAssertion(type="denied", name="python_execute"),
            ],
            config={"capability_policy": {"execute": "deny"}},
        )
        runner = EvalRunner()
        result = await runner.run_one(scenario)
        assert result.passed

    async def test_max_iterations_assertion(self):
        """Verify max_iterations stops the loop."""
        scenario = EvalScenario(
            id="test_max_iter",
            name="Max iterations",
            input="Loop forever",
            script=[
                [{"name": "list_files", "arguments": {"path": "."}}],
                [{"name": "list_files", "arguments": {"path": "."}}],
                [{"name": "list_files", "arguments": {"path": "."}}],
            ],
            assertions=[
                ScenarioAssertion(type="max_iterations"),
            ],
            config={"limits": {"max_iterations": 2}},
        )
        runner = EvalRunner()
        result = await runner.run_one(scenario)
        assert result.passed
        assert result.finish_reason == "max_iterations"

    async def test_failing_assertion(self):
        """A scenario with a wrong assertion should fail."""
        scenario = EvalScenario(
            id="test_fail",
            name="Failing assertion",
            input="Hello",
            script=["Hi!"],
            assertions=[
                ScenarioAssertion(type="tool_called", name="read_file"),  # won't be called
            ],
        )
        runner = EvalRunner()
        result = await runner.run_one(scenario)
        assert not result.passed
        assert len(result.failed_assertions) == 1

    async def test_tag_filtering(self):
        """run_all with tags filters scenarios correctly."""
        scenarios = [
            EvalScenario(id="s1", name="S1", tags=["a"], input="x", script=["y"]),
            EvalScenario(id="s2", name="S2", tags=["b"], input="x", script=["y"]),
            EvalScenario(id="s3", name="S3", tags=["a", "b"], input="x", script=["y"]),
        ]
        runner = EvalRunner()
        results = await runner.run_all(scenarios, tags=["a"])
        assert len(results) == 2
        assert {r.scenario_id for r in results} == {"s1", "s3"}

    async def test_result_serialization(self):
        """ScenarioResult round-trips through to_dict/from_dict."""
        scenario = EvalScenario(
            id="test_serial",
            name="Serialization test",
            input="Hello",
            script=["Hi!"],
            assertions=[ScenarioAssertion(type="finish_reason", reason="stop")],
        )
        runner = EvalRunner()
        result = await runner.run_one(scenario)
        data = result.to_dict()
        restored = ScenarioResult.from_dict(data)
        assert restored.scenario_id == result.scenario_id
        assert restored.passed == result.passed
        assert restored.finish_reason == result.finish_reason


# --- Built-in scenario suite tests ---


class TestBuiltinScenarios:
    """Run all built-in scenarios and verify they pass."""

    async def test_all_scenarios_pass(self, workspace):
        """All built-in scenarios should pass with the current implementation."""
        runner = EvalRunner()
        results = await runner.run_all(ALL_SCENARIOS)
        failures = [r for r in results if not r.passed]
        if failures:
            details = []
            for r in failures:
                for ar in r.failed_assertions:
                    details.append(f"  {r.scenario_id}: {ar.assertion.describe()} → {ar.detail}")
                if r.error:
                    details.append(f"  {r.scenario_id}: ERROR: {r.error}")
            pytest.fail(
                f"{len(failures)} scenario(s) failed:\n" + "\n".join(details)
            )

    async def test_tool_selection_scenarios(self, workspace):
        """Tool selection scenarios pass."""
        runner = EvalRunner()
        results = await runner.run_all(TOOL_SELECTION_SCENARIOS)
        assert all(r.passed for r in results), (
            f"Failed: {[r.scenario_id for r in results if not r.passed]}"
        )

    async def test_safety_scenarios(self, workspace):
        """Safety scenarios pass."""
        runner = EvalRunner()
        results = await runner.run_all(SAFETY_SCENARIOS)
        assert all(r.passed for r in results), (
            f"Failed: {[r.scenario_id for r in results if not r.passed]}"
        )

    async def test_cost_limit_scenarios(self, workspace):
        """Cost/limit scenarios pass."""
        runner = EvalRunner()
        results = await runner.run_all(COST_LIMIT_SCENARIOS)
        assert all(r.passed for r in results), (
            f"Failed: {[r.scenario_id for r in results if not r.passed]}"
        )


# --- Replay & comparison tests ---


class TestReplayComparison:
    """Tests for trace replay and comparison logic."""

    def test_compare_identical_runs(self):
        """Comparing identical results shows no regressions."""
        results = [
            ScenarioResult(
                scenario_id="s1",
                scenario_name="Test 1",
                passed=True,
                severity="critical",
                tags=["test"],
                elapsed_ms=100.0,
                total_tokens=50,
            ),
        ]
        report = compare_runs(results, results, baseline_name="test")
        assert report.passed_gate
        assert len(report.regressions) == 0
        assert report.unchanged == 1

    def test_compare_detects_regression(self):
        """A scenario that was passing but now fails is a regression."""
        baseline = [
            ScenarioResult(
                scenario_id="s1",
                scenario_name="Test 1",
                passed=True,
                severity="critical",
                tags=["test"],
            ),
        ]
        current = [
            ScenarioResult(
                scenario_id="s1",
                scenario_name="Test 1",
                passed=False,
                severity="critical",
                tags=["test"],
                assertion_results=[
                    AssertionResult(
                        assertion=ScenarioAssertion(type="tool_called", name="read_file"),
                        passed=False,
                        detail="tool not called",
                    )
                ],
            ),
        ]
        report = compare_runs(baseline, current, baseline_name="test")
        assert not report.passed_gate
        assert report.has_critical_regression
        assert len(report.regressions) == 1
        assert report.regressions[0].scenario_id == "s1"

    def test_compare_detects_fix(self):
        """A scenario that was failing but now passes is a fix."""
        baseline = [
            ScenarioResult(
                scenario_id="s1",
                scenario_name="Test 1",
                passed=False,
                severity="important",
                tags=["test"],
            ),
        ]
        current = [
            ScenarioResult(
                scenario_id="s1",
                scenario_name="Test 1",
                passed=True,
                severity="important",
                tags=["test"],
            ),
        ]
        report = compare_runs(baseline, current, baseline_name="test")
        assert report.passed_gate
        assert len(report.fixes) == 1

    def test_non_critical_regression_passes_gate(self):
        """A regression on a non-critical scenario doesn't fail the gate."""
        baseline = [
            ScenarioResult(
                scenario_id="s1",
                scenario_name="Test 1",
                passed=True,
                severity="info",
                tags=["test"],
            ),
        ]
        current = [
            ScenarioResult(
                scenario_id="s1",
                scenario_name="Test 1",
                passed=False,
                severity="info",
                tags=["test"],
            ),
        ]
        report = compare_runs(baseline, current, baseline_name="test")
        assert report.passed_gate  # info severity doesn't block
        assert len(report.regressions) == 1

    def test_trace_store_roundtrip(self, tmp_path):
        """TraceStore saves and loads baselines correctly."""
        store = TraceStore(tmp_path / "evals")
        results = [
            ScenarioResult(
                scenario_id="s1",
                scenario_name="Test 1",
                passed=True,
                severity="critical",
                tags=["test"],
                elapsed_ms=42.0,
                total_tokens=100,
                finish_reason="stop",
                tools_called=["read_file"],
            ),
        ]
        store.save_baseline("test_baseline", results)
        loaded = store.load_baseline("test_baseline")
        assert loaded is not None
        assert len(loaded) == 1
        assert loaded[0].scenario_id == "s1"
        assert loaded[0].passed is True
        assert loaded[0].elapsed_ms == 42.0

    def test_trace_store_list_baselines(self, tmp_path):
        """TraceStore lists available baselines."""
        store = TraceStore(tmp_path / "evals")
        assert store.list_baselines() == []
        store.save_baseline("b1", [])
        store.save_baseline("b2", [])
        assert sorted(store.list_baselines()) == ["b1", "b2"]


# --- CI Gate tests ---


class TestCIGate:
    """Tests for the CI gate logic."""

    async def test_gate_passes_all_critical(self, workspace):
        """Gate passes when all critical scenarios pass."""
        from evals.gate import run_gate

        exit_code = await run_gate(
            scenarios=TOOL_SELECTION_SCENARIOS,
            data_dir="evals_data_test",
        )
        assert exit_code == 0

    async def test_gate_fails_on_critical_failure(self):
        """Gate fails when a critical scenario fails."""
        from evals.gate import run_gate

        bad_scenario = EvalScenario(
            id="gate_fail_test",
            name="Guaranteed failure",
            severity=Severity.CRITICAL,
            input="Hello",
            script=["Hi!"],
            assertions=[
                ScenarioAssertion(type="tool_called", name="nonexistent_tool"),
            ],
        )
        exit_code = await run_gate(
            scenarios=[bad_scenario],
            data_dir="evals_data_test",
        )
        assert exit_code == 1
