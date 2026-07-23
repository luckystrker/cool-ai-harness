"""Trace replay and model/prompt comparison.

Provides:
- TraceRecord: a saved agent run trace (events + config + metrics)
- TraceStore: load/save traces to JSON files
- compare_runs: compare two sets of scenario results (quality, latency, cost)
- ComparisonReport: structured diff between two eval runs

Use cases:
- Replay a saved trace with a different model/prompt and compare outcomes
- A/B test system prompts by running the same scenarios with different configs
- Track quality/latency/cost regressions over time
"""

from __future__ import annotations

import json
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from evals.scenario import ScenarioResult


@dataclass
class TraceRecord:
    """A saved agent run trace for replay.

    Contains everything needed to reproduce a run:
    - The scenario definition (input, script, assertions)
    - The config used (model, system_prompt, limits, etc.)
    - The collected events and metrics
    """

    scenario_id: str
    config: dict[str, Any]
    events: list[dict[str, Any]]
    result: dict[str, Any]  # ScenarioResult.to_dict()
    metadata: dict[str, Any] = field(default_factory=dict)

    def to_dict(self) -> dict[str, Any]:
        return {
            "scenario_id": self.scenario_id,
            "config": self.config,
            "events": self.events,
            "result": self.result,
            "metadata": self.metadata,
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> TraceRecord:
        return cls(
            scenario_id=data["scenario_id"],
            config=data.get("config", {}),
            events=data.get("events", []),
            result=data.get("result", {}),
            metadata=data.get("metadata", {}),
        )


class TraceStore:
    """Load/save traces and baselines to a directory.

    Directory structure:
        traces_dir/
            baselines/
                <name>.json       — baseline results (list of ScenarioResult dicts)
            traces/
                <run_id>/
                    <scenario_id>.json  — individual trace records
    """

    def __init__(self, base_dir: str | Path = "evals_data") -> None:
        self.base_dir = Path(base_dir)
        self.baselines_dir = self.base_dir / "baselines"
        self.traces_dir = self.base_dir / "traces"

    def ensure_dirs(self) -> None:
        self.baselines_dir.mkdir(parents=True, exist_ok=True)
        self.traces_dir.mkdir(parents=True, exist_ok=True)

    def save_baseline(self, name: str, results: list[ScenarioResult]) -> Path:
        """Save a set of results as a named baseline."""
        self.ensure_dirs()
        path = self.baselines_dir / f"{name}.json"
        data = {
            "name": name,
            "results": [r.to_dict() for r in results],
        }
        path.write_text(json.dumps(data, indent=2, ensure_ascii=False), encoding="utf-8")
        return path

    def load_baseline(self, name: str) -> list[ScenarioResult] | None:
        """Load a named baseline. Returns None if not found."""
        path = self.baselines_dir / f"{name}.json"
        if not path.exists():
            return None
        data = json.loads(path.read_text(encoding="utf-8"))
        return [ScenarioResult.from_dict(r) for r in data.get("results", [])]

    def save_trace(self, run_id: str, trace: TraceRecord) -> Path:
        """Save a single trace record under a run directory."""
        run_dir = self.traces_dir / run_id
        run_dir.mkdir(parents=True, exist_ok=True)
        path = run_dir / f"{trace.scenario_id}.json"
        path.write_text(json.dumps(trace.to_dict(), indent=2, ensure_ascii=False), encoding="utf-8")
        return path

    def load_traces(self, run_id: str) -> list[TraceRecord]:
        """Load all traces for a run."""
        run_dir = self.traces_dir / run_id
        if not run_dir.exists():
            return []
        traces = []
        for path in sorted(run_dir.glob("*.json")):
            data = json.loads(path.read_text(encoding="utf-8"))
            traces.append(TraceRecord.from_dict(data))
        return traces

    def list_baselines(self) -> list[str]:
        """List available baseline names."""
        if not self.baselines_dir.exists():
            return []
        return [p.stem for p in self.baselines_dir.glob("*.json")]


# --- Comparison ---


@dataclass
class ScenarioDiff:
    """Diff of a single scenario between two runs."""

    scenario_id: str
    scenario_name: str
    # Pass/fail change
    baseline_passed: bool
    current_passed: bool
    regressed: bool  # was passing, now failing
    fixed: bool  # was failing, now passing
    # Metric deltas
    latency_delta_ms: float = 0.0
    token_delta: int = 0
    # Details
    baseline_finish_reason: str = ""
    current_finish_reason: str = ""
    new_failures: list[str] = field(default_factory=list)  # assertion descriptions


@dataclass
class ComparisonReport:
    """Structured comparison between a baseline and current run."""

    baseline_name: str
    total_scenarios: int = 0
    passed_baseline: int = 0
    passed_current: int = 0
    regressions: list[ScenarioDiff] = field(default_factory=list)
    fixes: list[ScenarioDiff] = field(default_factory=list)
    unchanged: int = 0
    # Aggregate metrics
    avg_latency_baseline_ms: float = 0.0
    avg_latency_current_ms: float = 0.0
    total_tokens_baseline: int = 0
    total_tokens_current: int = 0
    # Gate verdict
    has_critical_regression: bool = False

    @property
    def passed_gate(self) -> bool:
        """True if no critical regressions (CI gate passes)."""
        return not self.has_critical_regression

    def summary(self) -> str:
        """Human-readable summary line."""
        parts = [
            f"Scenarios: {self.total_scenarios}",
            f"Baseline pass: {self.passed_baseline}/{self.total_scenarios}",
            f"Current pass: {self.passed_current}/{self.total_scenarios}",
        ]
        if self.regressions:
            parts.append(f"REGRESSIONS: {len(self.regressions)}")
        if self.fixes:
            parts.append(f"Fixed: {len(self.fixes)}")
        parts.append(f"Avg latency: {self.avg_latency_baseline_ms:.0f}ms -> {self.avg_latency_current_ms:.0f}ms")
        parts.append(f"Tokens: {self.total_tokens_baseline} -> {self.total_tokens_current}")
        verdict = "PASS" if self.passed_gate else "FAIL (critical regression)"
        parts.append(f"Gate: {verdict}")
        return " | ".join(parts)

    def to_dict(self) -> dict[str, Any]:
        return {
            "baseline_name": self.baseline_name,
            "total_scenarios": self.total_scenarios,
            "passed_baseline": self.passed_baseline,
            "passed_current": self.passed_current,
            "regressions": [
                {
                    "scenario_id": d.scenario_id,
                    "scenario_name": d.scenario_name,
                    "new_failures": d.new_failures,
                    "latency_delta_ms": d.latency_delta_ms,
                    "token_delta": d.token_delta,
                }
                for d in self.regressions
            ],
            "fixes": [
                {"scenario_id": d.scenario_id, "scenario_name": d.scenario_name}
                for d in self.fixes
            ],
            "unchanged": self.unchanged,
            "avg_latency_baseline_ms": round(self.avg_latency_baseline_ms, 2),
            "avg_latency_current_ms": round(self.avg_latency_current_ms, 2),
            "total_tokens_baseline": self.total_tokens_baseline,
            "total_tokens_current": self.total_tokens_current,
            "has_critical_regression": self.has_critical_regression,
            "passed_gate": self.passed_gate,
        }


def compare_runs(
    baseline: list[ScenarioResult],
    current: list[ScenarioResult],
    *,
    baseline_name: str = "baseline",
) -> ComparisonReport:
    """Compare current results against a baseline.

    Identifies regressions (was passing, now failing), fixes, and metric deltas.
    A critical regression is a regression on a scenario with severity='critical'.
    """
    baseline_map = {r.scenario_id: r for r in baseline}
    current_map = {r.scenario_id: r for r in current}

    all_ids = set(baseline_map.keys()) | set(current_map.keys())
    report = ComparisonReport(baseline_name=baseline_name, total_scenarios=len(all_ids))

    latencies_baseline: list[float] = []
    latencies_current: list[float] = []

    for sid in sorted(all_ids):
        b = baseline_map.get(sid)
        c = current_map.get(sid)

        if b:
            latencies_baseline.append(b.elapsed_ms)
            report.total_tokens_baseline += b.total_tokens
        if c:
            latencies_current.append(c.elapsed_ms)
            report.total_tokens_current += c.total_tokens

        if b and c:
            if b.passed:
                report.passed_baseline += 1
            if c.passed:
                report.passed_current += 1

            if b.passed and not c.passed:
                # Regression
                new_failures = [
                    ar.assertion.describe()
                    for ar in c.failed_assertions
                ]
                diff = ScenarioDiff(
                    scenario_id=sid,
                    scenario_name=c.scenario_name,
                    baseline_passed=True,
                    current_passed=False,
                    regressed=True,
                    fixed=False,
                    latency_delta_ms=c.elapsed_ms - b.elapsed_ms,
                    token_delta=c.total_tokens - b.total_tokens,
                    baseline_finish_reason=b.finish_reason,
                    current_finish_reason=c.finish_reason,
                    new_failures=new_failures,
                )
                report.regressions.append(diff)
                if c.severity == "critical":
                    report.has_critical_regression = True
            elif not b.passed and c.passed:
                # Fixed
                diff = ScenarioDiff(
                    scenario_id=sid,
                    scenario_name=c.scenario_name,
                    baseline_passed=False,
                    current_passed=True,
                    regressed=False,
                    fixed=True,
                    latency_delta_ms=c.elapsed_ms - b.elapsed_ms,
                    token_delta=c.total_tokens - b.total_tokens,
                )
                report.fixes.append(diff)
            else:
                report.unchanged += 1
        elif c and not b:
            # New scenario (not in baseline)
            if c.passed:
                report.passed_current += 1
            report.unchanged += 1
        elif b and not c:
            # Removed scenario
            if b.passed:
                report.passed_baseline += 1
            report.unchanged += 1

    report.avg_latency_baseline_ms = (
        sum(latencies_baseline) / len(latencies_baseline) if latencies_baseline else 0.0
    )
    report.avg_latency_current_ms = (
        sum(latencies_current) / len(latencies_current) if latencies_current else 0.0
    )

    return report
