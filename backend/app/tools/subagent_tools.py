"""Subagent tools: spawn_subagent for agent-initiated delegation (Фаза 2 §5).

Registers the ``spawn_subagent`` tool so the main agent loop can delegate
sub-tasks to specialized subagents. The tool creates an isolated subagent
run, awaits its completion, and returns the result as the tool output.
"""

from __future__ import annotations

from pydantic import Field

from app.tools.base import ToolArgs, ToolResult, register_tool


class SpawnSubagentArgs(ToolArgs):
    """Arguments for the spawn_subagent tool."""

    prompt: str = Field(description="The task/instructions for the subagent to execute.")
    role: str | None = Field(
        default=None,
        description="Name of a predefined subagent role (e.g. 'researcher', 'code-reviewer', 'summarizer'). If omitted, a generic subagent is used.",
    )
    profile: str | None = Field(
        default=None,
        description="Slug of an agent profile to use (e.g. 'coder', 'researcher', 'writer', 'dm'). Alternative to 'role'. If both are given, profile takes priority.",
    )
    model: str | None = Field(
        default=None,
        description="Override model for the subagent. If omitted, uses the role's model or the system default.",
    )


async def _spawn_subagent(
    prompt: str, role: str | None = None, profile: str | None = None, model: str | None = None
) -> ToolResult:
    """Spawn a subagent to handle a sub-task and return its result."""
    from sqlmodel import Session

    from app.agent.subagents import (
        create_subagent_run,
        execute_subagent,
        get_role_by_name,
        subagent_registry,
    )
    from app.core.db import engine
    from app.tools.context import get_run_context

    ctx = get_run_context()

    with Session(engine) as session:
        # Resolve profile by slug if provided (Фаза 3a §2 — cross-profile invocation).
        profile_id: int | None = None
        if profile:
            from app.agent.personalities.service import get_profile_by_slug

            profile_obj = get_profile_by_slug(session, profile)
            if profile_obj is None:
                return ToolResult.err(
                    f"Unknown agent profile: '{profile}'. Available profiles can be listed via the API."
                )
            profile_id = profile_obj.id

        # Resolve role by name if provided (and no profile override).
        role_obj = None
        if role and profile_id is None:
            role_obj = get_role_by_name(session, role)
            if role_obj is None:
                return ToolResult.err(
                    f"Unknown subagent role: '{role}'. Available roles can be listed via the API."
                )

        # Determine parent conversation/run from the run context. The executor
        # stamps the owning run/conversation ids onto the RunContext (Фаза 2 §5)
        # so the child subagent is attributed to the correct parent and inherits
        # its working directory. Fall back to conversation 1 when absent (tests).
        parent_conversation_id = ctx.conversation_id or 1
        parent_run_id = ctx.run_id

        # Create the subagent run.
        sa_run = create_subagent_run(
            session,
            prompt=prompt,
            parent_conversation_id=parent_conversation_id,
            role=role_obj,
            parent_run_id=parent_run_id,
            model_override=model,
            profile_id=profile_id,
        )

        import asyncio

        # Register and execute synchronously (await the result).
        task = asyncio.ensure_future(execute_subagent(sa_run.id))
        subagent_registry.register(sa_run.id, task)

        try:
            result = await task
        except asyncio.CancelledError:
            return ToolResult.err("Subagent was cancelled.")
        except Exception as exc:
            return ToolResult.err(f"Subagent failed: {exc}")

        if result:
            return ToolResult.ok(result, subagent_run_id=sa_run.id)
        else:
            # Check if it was an error.
            session.refresh(sa_run)
            if sa_run.error:
                return ToolResult.err(f"Subagent error: {sa_run.error}")
            return ToolResult.ok(
                sa_run.result_summary or "Subagent completed with no output.",
                subagent_run_id=sa_run.id,
            )


def register_subagent_tools() -> None:
    """Register subagent-related tools. Idempotent."""
    register_tool(
        name="spawn_subagent",
        description=(
            "Spawn a specialized subagent to handle a sub-task. The subagent runs "
            "independently with its own context and tools, then returns its result. "
            "Use this for delegating research, code review, summarization, or other "
            "specialized tasks. Available roles: researcher, code-reviewer, summarizer. "
            "You can also specify an agent profile slug (e.g. 'coder', 'writer', 'dm') "
            "to invoke another personality as a subagent."
        ),
        args_model=SpawnSubagentArgs,
        func=_spawn_subagent,
        dangerous=False,
    )
