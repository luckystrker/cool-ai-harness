"""CLI entry point for agent evals.

Usage:
    python -m evals                         Run all scenarios, gate on critical
    python -m evals --tag safety            Run only safety scenarios
    python -m evals --tag tool_selection    Run only tool selection scenarios
    python -m evals -v                      Verbose output (show assertion details)
    python -m evals --baseline default      Compare against saved baseline
    python -m evals --update-baseline       Save current results as baseline
    python -m evals --data-dir ./evals_data Custom data directory
    python -m evals --json                  Output results as JSON (for CI)
"""

from __future__ import annotations

import argparse
import asyncio
import sys


def main() -> None:
    parser = argparse.ArgumentParser(
        prog="evals",
        description="Agent eval scenarios: tool selection, safety, cost limits.",
    )
    parser.add_argument(
        "--tag", "-t",
        action="append",
        dest="tags",
        help="Filter scenarios by tag (can be repeated)",
    )
    parser.add_argument(
        "--baseline", "-b",
        default=None,
        help="Compare results against a named baseline",
    )
    parser.add_argument(
        "--update-baseline",
        action="store_true",
        help="Save current results as the named baseline (default: 'default')",
    )
    parser.add_argument(
        "--data-dir",
        default="evals_data",
        help="Directory for baselines and traces (default: evals_data)",
    )
    parser.add_argument(
        "--verbose", "-v",
        action="store_true",
        help="Show detailed per-scenario assertion results",
    )
    parser.add_argument(
        "--json",
        action="store_true",
        help="Output results as JSON (for CI integration)",
    )

    args = parser.parse_args()

    exit_code = asyncio.run(
        _run(args),
    )
    sys.exit(exit_code)


async def _run(args: argparse.Namespace) -> int:
    from evals.gate import run_gate

    return await run_gate(
        tags=args.tags,
        baseline_name=args.baseline,
        update_baseline=args.update_baseline,
        data_dir=args.data_dir,
        verbose=args.verbose,
    )


if __name__ == "__main__":
    main()
