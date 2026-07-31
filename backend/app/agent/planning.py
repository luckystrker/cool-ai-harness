"""Planning Mode service (Фаза 2 §1).

Plan mode runs the full agent loop with a planning-specific system prompt.
The agent researches the task using tools (read files, explore code), then
outputs a structured JSON plan wrapped in ```plan ... ``` markers. The plan
is extracted from the final response, persisted, and rendered as an
interactive PlanCard for user approval.

During build-mode execution, the active plan is injected into the agent's
context and the agent uses the ``plan_step_update`` tool to mark progress.
"""

from __future__ import annotations

import json
import re
from collections.abc import AsyncIterator
from typing import Any

from sqlmodel import Session, select

from app.agent.events import AgentEvent
from app.core.logging import get_logger
from app.models.plan import (
    PLAN_STATUS_APPROVED,
    PLAN_STATUS_CANCELLED,
    PLAN_STATUS_COMPLETED,
    PLAN_STATUS_DRAFT,
    PLAN_STATUS_EXECUTING,
    PLAN_STATUS_FAILED,
    STEP_STATUS_COMPLETED,
    STEP_STATUS_FAILED,
    STEP_STATUS_PENDING,
    STEP_STATUS_RUNNING,
    STEP_STATUS_SKIPPED,
    Plan,
    PlanStep,
    PlanTemplate,
)
from app.providers import LLMProvider, Message

log = get_logger(__name__)

# --- Planning system prompt ---

PLANNING_SYSTEM_PROMPT = """\
You are in PLANNING MODE. Your job is to produce a detailed, well-researched \
execution plan for the user's request.

## Process
1. RESEARCH: Use your tools to thoroughly investigate the task. Read relevant \
documentation, source files, and configuration. Understand the current state \
of the codebase and what specifically needs to change.
2. ANALYZE: Based on your research, identify the concrete files, functions, \
and components that need modification.
3. PLAN: Output a structured plan as your FINAL message.

## Output format
Your final message MUST end with a plan block in this exact format:

```plan
{"title": "...", "steps": [{"position": 0, "title": "...", "description": "...", "depends_on": [], "tools": []}]}
```

Rules for steps:
- Each step must be SPECIFIC (reference actual file paths, function names, etc.)
- Steps are ordered by "position" (0-based).
- "depends_on" lists positions that must complete first.
- "tools" lists suggested tool names for the step.
- "delegate_role" (optional) names a subagent role to delegate the step to \
(e.g. "researcher", "code-reviewer"). When set, a subagent executes the step.
- 3-10 steps depending on complexity.
- Each step must be independently verifiable.

IMPORTANT: Do NOT output the plan until you have completed your research. \
Generic steps like "gather requirements" or "review and revise" are NOT \
acceptable. Every step must reference concrete artifacts you discovered.
"""


def planning_system_prompt() -> str:
    """Return the system prompt used for plan generation."""
    return PLANNING_SYSTEM_PROMPT


# --- Plan extraction ---

_PLAN_BLOCK_RE = re.compile(r"```plan\s*\n(.*?)\n```", re.DOTALL)


def extract_plan_from_response(content: str) -> dict[str, Any] | None:
    """Extract a plan JSON from a ```plan ... ``` block in the response.

    Returns the parsed plan dict or None if no valid plan block is found.
    """
    match = _PLAN_BLOCK_RE.search(content)
    if not match:
        return None
    try:
        plan_data = json.loads(match.group(1))
    except json.JSONDecodeError:
        log.warning("planning.extract_parse_failed", content=match.group(1)[:300])
        return None
    if not isinstance(plan_data, dict) or "steps" not in plan_data:
        return None
    if not isinstance(plan_data["steps"], list) or len(plan_data["steps"]) == 0:
        return None
    # Normalize steps: ensure position is set.
    for i, step in enumerate(plan_data["steps"]):
        if "position" not in step:
            step["position"] = i
        if "title" not in step:
            step["title"] = f"Step {i + 1}"
    return plan_data


# --- Plan context injection (build mode) ---


def get_active_plan(session: Session, conversation_id: int) -> Plan | None:
    """Return the active (approved/executing) plan for a conversation, if any."""
    return session.exec(
        select(Plan)
        .where(Plan.conversation_id == conversation_id)
        .where(Plan.status.in_([PLAN_STATUS_APPROVED, PLAN_STATUS_EXECUTING]))  # type: ignore[union-attr]
        .order_by(Plan.id.desc())  # type: ignore[union-attr]
    ).first()


def build_plan_context(session: Session, conversation_id: int) -> str | None:
    """Build a plan context string to inject into the system prompt.

    Returns None if there is no active plan for the conversation.
    """
    plan = get_active_plan(session, conversation_id)
    if plan is None:
        return None
    steps = get_plan_steps(session, plan.id)  # type: ignore[arg-type]
    if not steps:
        return None

    status_marks = {
        STEP_STATUS_COMPLETED: "[x]",
        STEP_STATUS_FAILED: "[!]",
        STEP_STATUS_SKIPPED: "[-]",
        STEP_STATUS_RUNNING: "[>]",
        STEP_STATUS_PENDING: "[ ]",
    }
    lines = [f"## Active Plan: {plan.title or 'Untitled'} (id={plan.id})", "Steps:"]
    for s in steps:
        mark = status_marks.get(s.status, "[ ]")
        line = f"{s.position + 1}. {mark} {s.title}"
        if s.status == STEP_STATUS_COMPLETED and s.result_summary:
            line += f' — done: "{s.result_summary[:80]}"'
        elif s.status == STEP_STATUS_FAILED and s.result_summary:
            line += f' — failed: "{s.result_summary[:80]}"'
        if s.depends_on:
            deps = ", ".join(str(d + 1) for d in s.depends_on)
            line += f" (depends on: {deps})"
        lines.append(line)
    lines.append("")
    lines.append("Use the plan_step_update tool to mark steps as you complete them.")
    return "\n".join(lines)


# --- CRUD helpers ---


def create_plan(
    session: Session,
    *,
    conversation_id: int,
    run_id: int | None = None,
    title: str | None = None,
    steps: list[dict[str, Any]],
    metadata_: dict[str, Any] | None = None,
) -> Plan:
    """Persist a new plan and its steps."""
    plan = Plan(
        conversation_id=conversation_id,
        run_id=run_id,
        title=title,
        status=PLAN_STATUS_DRAFT,
        steps=steps,
        metadata_=metadata_,
    )
    session.add(plan)
    session.commit()
    session.refresh(plan)

    # Create individual step rows.
    for step_data in steps:
        row = PlanStep(
            plan_id=plan.id,  # id is set after refresh
            position=step_data.get("position", 0),
            title=step_data.get("title", "Untitled"),
            description=step_data.get("description"),
            status=STEP_STATUS_PENDING,
            depends_on=step_data.get("depends_on"),
            tools=step_data.get("tools"),
            delegate_role=step_data.get("delegate_role"),
        )
        session.add(row)
    session.commit()
    return plan


def get_plan(session: Session, plan_id: int) -> Plan | None:
    return session.get(Plan, plan_id)


def list_plans(session: Session, *, conversation_id: int) -> list[Plan]:
    """Plans for a conversation, newest first."""
    return list(
        session.exec(
            select(Plan)
            .where(Plan.conversation_id == conversation_id)
            .order_by(Plan.id.desc())  # type: ignore[union-attr]
        ).all()
    )


def get_plan_steps(session: Session, plan_id: int) -> list[PlanStep]:
    """Steps for a plan, ordered by position."""
    return list(
        session.exec(
            select(PlanStep).where(PlanStep.plan_id == plan_id).order_by(PlanStep.position)  # type: ignore[arg-type]
        ).all()
    )


def update_plan_steps(
    session: Session,
    plan_id: int,
    *,
    title: str | None = None,
    steps: list[dict[str, Any]] | None = None,
) -> Plan | None:
    """Edit a draft plan's title and/or steps (before approval).

    Replaces all step rows with the new step list.
    """
    plan = session.get(Plan, plan_id)
    if plan is None or plan.status != PLAN_STATUS_DRAFT:
        return None

    if title is not None:
        plan.title = title

    if steps is not None:
        # Delete old step rows.
        old_steps = session.exec(select(PlanStep).where(PlanStep.plan_id == plan_id)).all()
        for old in old_steps:
            session.delete(old)
        # Create new step rows.
        for step_data in steps:
            row = PlanStep(
                plan_id=plan_id,
                position=step_data.get("position", 0),
                title=step_data.get("title", "Untitled"),
                description=step_data.get("description"),
                status=STEP_STATUS_PENDING,
                depends_on=step_data.get("depends_on"),
                tools=step_data.get("tools"),
                delegate_role=step_data.get("delegate_role"),
            )
            session.add(row)
        plan.steps = steps

    session.add(plan)
    session.commit()
    session.refresh(plan)
    return plan


def approve_plan(session: Session, plan_id: int) -> Plan | None:
    """Transition a draft plan to approved status."""
    plan = session.get(Plan, plan_id)
    if plan is None or plan.status != PLAN_STATUS_DRAFT:
        return None
    plan.status = PLAN_STATUS_APPROVED
    session.add(plan)
    session.commit()
    session.refresh(plan)
    return plan


def cancel_plan(session: Session, plan_id: int) -> Plan | None:
    """Cancel a plan (from any non-terminal status)."""
    plan = session.get(Plan, plan_id)
    if plan is None or plan.status in (PLAN_STATUS_COMPLETED, PLAN_STATUS_FAILED, PLAN_STATUS_CANCELLED):
        return None
    plan.status = PLAN_STATUS_CANCELLED
    session.add(plan)
    session.commit()
    session.refresh(plan)
    return plan


# --- Plan execution ---


def _topological_order(steps: list[PlanStep]) -> list[PlanStep]:
    """Sort steps respecting depends_on edges (Kahn's algorithm).

    Falls back to position order for steps with unresolved dependencies.
    """
    by_position = {s.position: s for s in steps}
    in_degree: dict[int, int] = {s.position: 0 for s in steps}
    dependents: dict[int, list[int]] = {s.position: [] for s in steps}

    for s in steps:
        for dep in s.depends_on or []:
            if dep in by_position:
                in_degree[s.position] = in_degree.get(s.position, 0) + 1
                dependents.setdefault(dep, []).append(s.position)

    queue = sorted([pos for pos, deg in in_degree.items() if deg == 0])
    order: list[PlanStep] = []

    while queue:
        pos = queue.pop(0)
        order.append(by_position[pos])
        for dependent in dependents.get(pos, []):
            in_degree[dependent] -= 1
            if in_degree[dependent] == 0:
                queue.append(dependent)
        queue.sort()

    # Append any remaining (cycle) steps in position order.
    seen = {s.position for s in order}
    for s in sorted(steps, key=lambda x: x.position):
        if s.position not in seen:
            order.append(s)

    return order


async def _execute_step_via_subagent(
    session: Session,
    *,
    step: PlanStep,
    prompt: str,
    role_name: str,
    parent_conversation_id: int,
    working_directory: str | None = None,
) -> tuple[str, bool]:
    """Execute a plan step by delegating to a subagent with the given role.

    Returns (result_summary, failed).
    """
    from app.agent.subagents import (
        create_subagent_run,
        execute_subagent,
        get_role_by_name,
    )

    role = get_role_by_name(session, role_name)
    if role is None:
        log.warning("planning.delegate_role_not_found", role=role_name, step=step.position)
        return f"Failed: subagent role '{role_name}' not found", True

    try:
        sa_run = create_subagent_run(
            session,
            prompt=prompt,
            parent_conversation_id=parent_conversation_id,
            role=role,
            name=f"plan-step-{step.position}:{role_name}",
        )
        result = await execute_subagent(sa_run.id)  # type: ignore[arg-type]
        if result:
            return result[:500], False
        # Check if the run failed.
        session.refresh(sa_run)
        if sa_run.error:
            return f"Failed: {sa_run.error[:300]}", True
        return sa_run.result_summary or "Completed via subagent", False
    except Exception as exc:
        log.error("planning.subagent_step_failed", step=step.position, error=str(exc))
        return f"Failed: {exc!s}"[:500], True


async def execute_plan_steps(
    session: Session,
    plan: Plan,
    *,
    provider: LLMProvider,
    model: str,
    history: list[Message],
    working_directory: str | None = None,
) -> AsyncIterator[AgentEvent]:
    """Execute an approved plan step-by-step, yielding progress events.

    Each step is executed as a focused agent sub-turn using the normal
    AgentExecutor. Step results are summarized and appended to the
    conversation history for subsequent steps.
    """
    from app.agent.executor import AgentConfig, AgentExecutor, AgentLimits

    plan.status = PLAN_STATUS_EXECUTING
    session.add(plan)
    session.commit()

    steps = get_plan_steps(session, plan.id)  # type: ignore[arg-type]
    ordered = _topological_order(steps)
    total = len(ordered)
    completed_count = 0
    failed = False

    yield AgentEvent.plan_progress(completed=0, total=total, current_step=None)

    # Working history accumulates step results for context.
    exec_history = list(history)

    for step in ordered:
        # Check if dependencies are satisfied.
        deps_ok = True
        for dep_pos in step.depends_on or []:
            dep_step = next((s for s in steps if s.position == dep_pos), None)
            if dep_step and dep_step.status not in (STEP_STATUS_COMPLETED, STEP_STATUS_SKIPPED):
                deps_ok = False
                break

        if not deps_ok:
            step.status = STEP_STATUS_SKIPPED
            session.add(step)
            session.commit()
            yield AgentEvent.plan_step_complete(
                position=step.position, status=STEP_STATUS_SKIPPED, result_summary="Skipped: dependencies not met"
            )
            continue

        # Mark step running.
        step.status = STEP_STATUS_RUNNING
        session.add(step)
        session.commit()
        yield AgentEvent.plan_step_start(position=step.position, title=step.title)

        # Build the step prompt.
        step_prompt = f"Execute this plan step:\n\n**{step.title}**\n"
        if step.description:
            step_prompt += f"\n{step.description}\n"
        step_prompt += "\nComplete this step and provide a brief summary of what was accomplished."

        # Delegate to subagent if delegate_role is set.
        if step.delegate_role:
            summary, step_failed = await _execute_step_via_subagent(
                session,
                step=step,
                prompt=step_prompt,
                role_name=step.delegate_role,
                parent_conversation_id=plan.conversation_id,
                working_directory=working_directory,
            )
        else:
            # Execute via a mini agent loop (limited iterations per step).
            step_config = AgentConfig(
                model=model,
                system_prompt=None,
                tool_names=None,
                limits=AgentLimits(max_iterations=5),
                working_directory=working_directory,
                auto_approve=True,  # Plan steps run without per-tool approval.
            )
            executor = AgentExecutor(
                provider=provider,
                config=step_config,
                history=list(exec_history),
            )

            step_result_parts: list[str] = []
            step_failed = False
            try:
                async for event in executor.stream(step_prompt):
                    if event.kind == "token":
                        step_result_parts.append(event.payload.get("text", ""))
                    elif event.kind == "error" or (
                        event.kind == "finish" and event.payload.get("reason") == "error"
                    ):
                        step_failed = True
            except Exception as exc:
                log.error("planning.step_failed", step=step.position, error=str(exc))
                step_failed = True

            # Summarize the step result.
            result_text = "".join(step_result_parts).strip()
            summary = result_text[:500] if result_text else ("Failed" if step_failed else "Completed")

        if step_failed:
            step.status = STEP_STATUS_FAILED
            step.result_summary = summary
            session.add(step)
            session.commit()
            failed = True
            yield AgentEvent.plan_step_complete(
                position=step.position, status=STEP_STATUS_FAILED, result_summary=summary
            )
            # Stop execution on failure.
            break
        else:
            step.status = STEP_STATUS_COMPLETED
            step.result_summary = summary
            session.add(step)
            session.commit()
            completed_count += 1

            # Append step result to history for subsequent steps.
            exec_history.append(Message(role="assistant", content=f"[Step: {step.title}]\n{summary}"))

            yield AgentEvent.plan_step_complete(
                position=step.position, status=STEP_STATUS_COMPLETED, result_summary=summary
            )

        yield AgentEvent.plan_progress(completed=completed_count, total=total, current_step=step.position)

    # Finalize plan status.
    plan.status = PLAN_STATUS_FAILED if failed else PLAN_STATUS_COMPLETED
    session.add(plan)
    session.commit()

    yield AgentEvent.plan_progress(completed=completed_count, total=total, current_step=None)


# --- Plan templates ---


def list_templates(session: Session) -> list[PlanTemplate]:
    return list(session.exec(select(PlanTemplate).order_by(PlanTemplate.id)).all())  # type: ignore[arg-type]


def create_template(
    session: Session,
    *,
    name: str,
    description: str | None = None,
    steps: list[dict[str, Any]],
    is_builtin: bool = False,
) -> PlanTemplate:
    tpl = PlanTemplate(name=name, description=description, steps=steps, is_builtin=is_builtin)
    session.add(tpl)
    session.commit()
    session.refresh(tpl)
    return tpl


def delete_template(session: Session, template_id: int) -> bool:
    tpl = session.get(PlanTemplate, template_id)
    if tpl is None or tpl.is_builtin:
        return False
    session.delete(tpl)
    session.commit()
    return True
