"""Cost & limit scenarios: verify the agent respects iteration/token/cost budgets.

These scenarios test:
- max_iterations enforcement
- token budget enforcement
- Proper finish reasons when limits are hit
"""

from evals.scenario import EvalScenario, ScenarioAssertion, Severity

COST_LIMIT_SCENARIOS: list[EvalScenario] = [
    # --- Max iterations ---
    EvalScenario(
        id="cost_max_iterations",
        name="Max iterations enforcement",
        description="Agent stops after max_iterations even if the model keeps calling tools",
        tags=["cost", "limits", "iterations"],
        severity=Severity.CRITICAL,
        input="Keep processing files forever",
        script=[
            # The model keeps calling tools indefinitely — the loop must stop at max_iterations
            [{"name": "list_files", "arguments": {"path": "."}}],
            [{"name": "list_files", "arguments": {"path": "."}}],
            [{"name": "list_files", "arguments": {"path": "."}}],
            [{"name": "list_files", "arguments": {"path": "."}}],
            [{"name": "list_files", "arguments": {"path": "."}}],
        ],
        assertions=[
            ScenarioAssertion(type="max_iterations"),
            ScenarioAssertion(type="finish_reason", reason="max_iterations"),
        ],
        config={
            "limits": {"max_iterations": 3},
        },
    ),
    # --- Token budget ---
    EvalScenario(
        id="cost_token_budget",
        name="Token budget enforcement",
        description="Agent stops when total tokens exceed the budget",
        tags=["cost", "limits", "tokens"],
        severity=Severity.CRITICAL,
        input="Process a large dataset",
        script=[
            # Each scripted turn reports 15 tokens (10 prompt + 5 completion).
            # With max_total_tokens=25, the loop should stop after 2 iterations
            # (15 tokens after iter 1, 30 after iter 2 >= 25).
            [{"name": "read_file", "arguments": {"path": "data.csv"}}],
            [{"name": "read_file", "arguments": {"path": "data2.csv"}}],
            "Processing complete.",
        ],
        assertions=[
            ScenarioAssertion(type="finish_reason", reason="token_limit"),
            ScenarioAssertion(type="token_budget", max_tokens=30),
        ],
        config={
            "limits": {"max_iterations": 10, "max_total_tokens": 25},
        },
    ),
    # --- Iteration limit with tool chain ---
    EvalScenario(
        id="cost_iteration_limit_tool_chain",
        name="Iteration limit during tool chain",
        description="Agent respects iteration limit even mid-tool-chain",
        tags=["cost", "limits", "iterations"],
        severity=Severity.IMPORTANT,
        input="Read all files one by one",
        script=[
            [{"name": "read_file", "arguments": {"path": "a.txt"}}],
            [{"name": "read_file", "arguments": {"path": "b.txt"}}],
            [{"name": "read_file", "arguments": {"path": "c.txt"}}],
        ],
        assertions=[
            ScenarioAssertion(type="finish_reason", reason="max_iterations"),
            # Should have called read_file exactly 2 times (max_iterations=2)
            ScenarioAssertion(type="tool_called", name="read_file"),
        ],
        config={
            "limits": {"max_iterations": 2},
        },
    ),
    # --- Normal completion within budget ---
    EvalScenario(
        id="cost_within_budget",
        name="Completes within budget",
        description="Agent completes normally when within all limits",
        tags=["cost", "limits", "normal"],
        severity=Severity.INFO,
        input="Read one file",
        script=[
            [{"name": "read_file", "arguments": {"path": "hello.txt"}}],
            "The file says hello.",
        ],
        assertions=[
            ScenarioAssertion(type="finish_reason", reason="stop"),
            ScenarioAssertion(type="tool_called", name="read_file"),
        ],
        config={
            "limits": {"max_iterations": 10, "max_total_tokens": 10000},
        },
    ),
    # --- Single iteration limit ---
    EvalScenario(
        id="cost_single_iteration",
        name="Single iteration limit",
        description="With max_iterations=1, agent runs one LLM call and stops",
        tags=["cost", "limits", "iterations"],
        severity=Severity.IMPORTANT,
        input="Do something complex",
        script=[
            [{"name": "python_execute", "arguments": {"code": "print('step 1')"}}],
            # This second turn should never be reached
            "This should not appear",
        ],
        assertions=[
            ScenarioAssertion(type="finish_reason", reason="max_iterations"),
            ScenarioAssertion(type="tool_called", name="python_execute"),
        ],
        config={
            "limits": {"max_iterations": 1},
        },
    ),
]
