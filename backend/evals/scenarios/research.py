"""Deep research scenarios: verify the agent selects the deep_research tool.

The scenario scripts the model to call ``deep_research`` for an investigation
request (the tool itself runs against real infra only when a provider is
configured; in the eval gate it degrades to a graceful tool error, which the
loop survives — the assertion is about tool selection).
"""

from evals.scenario import EvalScenario, ScenarioAssertion, Severity

RESEARCH_SCENARIOS: list[EvalScenario] = [
    EvalScenario(
        id="research_select_deep_research_tool",
        name="Deep research tool selection",
        description="Agent calls deep_research when the user asks for thorough multi-source investigation",
        tags=["research", "tool_selection"],
        severity=Severity.IMPORTANT,
        input="Do a deep research on the history of the Alpha framework and write a cited report",
        script=[
            [
                {
                    "name": "deep_research",
                    "arguments": {"topic": "history of the Alpha framework"},
                }
            ],
            "I've completed the deep research. Here is the cited report.",
        ],
        # Capability-denied: the eval stays deterministic (no real network or
        # provider calls); tool selection is still asserted via tool_call_start.
        config={"capability_policy": {"network": "deny", "execute": "deny"}},
        assertions=[
            ScenarioAssertion(type="tool_called", name="deep_research"),
            ScenarioAssertion(
                type="tool_called",
                name="deep_research",
                arguments={"topic": "history of the Alpha framework"},
            ),
            ScenarioAssertion(type="finish_reason", reason="stop"),
        ],
    ),
    EvalScenario(
        id="research_deep_research_accepts_depth",
        name="Deep research depth parameter",
        description="Agent passes the depth parameter to deep_research",
        tags=["research", "tool_selection"],
        severity=Severity.INFO,
        input="Research quantum computing trends in depth across 5 sub-questions",
        script=[
            [
                {
                    "name": "deep_research",
                    "arguments": {"topic": "quantum computing trends", "depth": 5},
                }
            ],
            "The research is complete.",
        ],
        config={"capability_policy": {"network": "deny", "execute": "deny"}},
        assertions=[
            ScenarioAssertion(
                type="tool_called",
                name="deep_research",
                arguments={"topic": "quantum computing trends", "depth": 5},
            ),
            ScenarioAssertion(type="finish_reason", reason="stop"),
        ],
    ),
]
