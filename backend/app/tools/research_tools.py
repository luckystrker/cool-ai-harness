"""Deep Research tool: agent-initiated research workflow (Фаза 4).

Registers the ``deep_research`` tool so the main agent loop can run the full
research pipeline (decompose → parallel researcher subagents → synthesize with
citations) and receive the report as the tool output. The pipeline is durable:
a ResearchRun row tracks status/sources/citations, and the report is stored in
the artifact library.
"""

from __future__ import annotations

from pydantic import Field

from app.tools.base import ToolArgs, ToolResult, register_tool


class DeepResearchArgs(ToolArgs):
    """Arguments for the deep_research tool."""

    topic: str = Field(description="The research question or topic to investigate thoroughly.")
    depth: int = Field(
        default=4,
        description="How many sub-questions to decompose the topic into (3-5). Default 4.",
        ge=3,
        le=5,
    )
    model: str | None = Field(
        default=None,
        description="Override model for the research pipeline. If omitted, the default model is used.",
    )


async def _deep_research(topic: str, depth: int = 4, model: str | None = None) -> ToolResult:
    """Run the deep research pipeline and return the synthesized report."""
    from app.research import run_research_inline
    from app.tools.context import get_run_context

    ctx = get_run_context()
    try:
        report, run_id = await run_research_inline(
            topic=topic,
            depth=depth,
            model=model,
            conversation_id=ctx.conversation_id,
        )
    except Exception as exc:
        return ToolResult.err(f"Deep research failed: {exc}")

    if not report:
        return ToolResult.err(
            "Deep research completed without a report (see research runs in the UI).",
            research_run_id=run_id,
        )
    return ToolResult.ok(
        report,
        research_run_id=run_id,
        format="markdown",
    )


def register_research_tools() -> None:
    """Register deep-research-related tools. Idempotent."""
    from app.security.capabilities import Capability

    register_tool(
        name="deep_research",
        description=(
            "Conduct deep multi-source research on a topic: decompose it into "
            "sub-questions, run parallel researcher subagents that search the "
            "web and read sources, then synthesize a structured report with "
            "inline citations and a bibliography. Use this for questions that "
            "need thorough, cited investigation rather than a quick answer. "
            "The report is also saved to the artifact library."
        ),
        args_model=DeepResearchArgs,
        func=_deep_research,
        dangerous=True,
        capabilities=frozenset({Capability.NETWORK, Capability.EXECUTE}),
    )
