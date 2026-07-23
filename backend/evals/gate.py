"""CI quality gate: run evals and fail on critical regressions.

This module provides the gate logic used by the CLI (`python -m evals`) and
can be integrated into CI pipelines (GitHub Actions, etc.).

Exit codes:
    0 — all critical scenarios pass (gate passes)
    1 — at least one critical scenario regressed or failed
    2 — configuration / runtime error
"""

from __future__ import annotations

from pathlib import Path

from evals.replay import ComparisonReport, TraceStore, compare_runs
from evals.runner import EvalRunner
from evals.scenario import ScenarioResult


async def run_gate(
    *,
    tags: list[str] | None = None,
    baseline_name: str | None = None,
    update_baseline: bool = False,
    data_dir: str | Path = "evals_data",
    verbose: bool = False,
    scenarios: list | None = None,
) -> int:
    """Run the eval gate. Returns an exit code (0=pass, 1=fail, 2=error).

    Args:
        tags: filter scenarios by tags (None = all)
        baseline_name: compare against this baseline (None = just run, no comparison)
        update_baseline: save current results as the named baseline
        data_dir: directory for baselines and traces
        verbose: print detailed per-scenario output
        scenarios: override scenario list (default: ALL_SCENARIOS)
    """
    from evals.scenarios import ALL_SCENARIOS

    if scenarios is None:
        scenarios = ALL_SCENARIOS

    store = TraceStore(data_dir)
    runner = EvalRunner()

    # Run all scenarios
    print(f"Running {len(scenarios)} eval scenarios...")
    if tags:
        print(f"  Filtered by tags: {tags}")
    print()

    results = await runner.run_all(scenarios, tags=tags)

    # Print results
    _print_results(results, verbose=verbose)

    # Save baseline if requested
    if update_baseline:
        name = baseline_name or "default"
        path = store.save_baseline(name, results)
        print(f"\nBaseline '{name}' saved to {path}")

    # Compare against baseline if requested
    if baseline_name and not update_baseline:
        baseline = store.load_baseline(baseline_name)
        if baseline is None:
            print(f"\nERROR: Baseline '{baseline_name}' not found.")
            print(f"Available baselines: {store.list_baselines()}")
            print("Run with --update-baseline first to create one.")
            return 2

        report = compare_runs(baseline, results, baseline_name=baseline_name)
        _print_comparison(report, verbose=verbose)

        if not report.passed_gate:
            print("\n[FAIL] GATE FAILED: critical regression detected")
            return 1
        print("\n[OK] GATE PASSED")
        return 0

    # No baseline comparison — gate passes if all critical scenarios pass
    critical_failures = [
        r for r in results if r.severity == "critical" and not r.passed
    ]
    if critical_failures:
        print(f"\n[FAIL] GATE FAILED: {len(critical_failures)} critical scenario(s) failed")
        for r in critical_failures:
            print(f"  - {r.scenario_id}: {r.scenario_name}")
            for ar in r.failed_assertions:
                print(f"    [x] {ar.assertion.describe()}: {ar.detail}")
        return 1

    print("\n[OK] GATE PASSED: all critical scenarios pass")
    return 0


def _print_results(results: list[ScenarioResult], *, verbose: bool = False) -> None:
    """Print a summary table of results."""
    passed = sum(1 for r in results if r.passed)
    failed = len(results) - passed

    print(f"{'ID':<40} {'Status':<8} {'Time':<10} {'Tokens':<8} {'Severity':<10}")
    print("-" * 80)
    for r in results:
        status = "PASS" if r.passed else "FAIL"
        print(
            f"{r.scenario_id:<40} {status:<8} {r.elapsed_ms:>7.1f}ms "
            f"{r.total_tokens:<8} {r.severity:<10}"
        )
        if verbose and not r.passed:
            for ar in r.failed_assertions:
                print(f"    [x] {ar.assertion.describe()}")
                if ar.detail:
                    print(f"      -> {ar.detail}")
            if r.error:
                print(f"    ERROR: {r.error}")
        elif verbose:
            if r.tools_called:
                print(f"    tools: {', '.join(r.tools_called)}")

    print("-" * 80)
    print(f"Total: {len(results)} | Passed: {passed} | Failed: {failed}")


def _print_comparison(report: ComparisonReport, *, verbose: bool = False) -> None:
    """Print comparison report."""
    print(f"\n{'='*60}")
    print(f"COMPARISON vs baseline '{report.baseline_name}'")
    print(f"{'='*60}")
    print(report.summary())

    if report.regressions:
        print(f"\nRegressions ({len(report.regressions)}):")
        for d in report.regressions:
            print(f"  [x] {d.scenario_id} ({d.scenario_name})")
            for f in d.new_failures:
                print(f"    - {f}")
            if verbose:
                print(f"    latency: {d.latency_delta_ms:+.1f}ms, tokens: {d.token_delta:+d}")

    if report.fixes:
        print(f"\nFixed ({len(report.fixes)}):")
        for d in report.fixes:
            print(f"  [v] {d.scenario_id} ({d.scenario_name})")
