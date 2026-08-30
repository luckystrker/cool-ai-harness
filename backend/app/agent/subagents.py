"""Subagent orchestration service (Фаза 2 §5 — Subagents).

Manages the lifecycle of subagent runs: creation of isolated conversations,
execution via the shared ``run_conversation_turn`` runner, cancellation,
and status tracking. Each subagent gets its own Conversation (separate
history) and AgentRun (durable execution), with ``auto_approve=True``
so no human-in-the-loop is needed for sub-tasks.

Concurrent subagents are tracked in an in-memory registry (asyncio tasks)
so the API layer can launch multiple simultaneously and cancel them.
"""

from __future__ import annotations

import asyncio
from collections.abc import Sequence
from datetime import UTC, datetime
from typing import Any

from sqlmodel import Session, col, select

from app.agent.runners import run_conversation_turn
from app.agent.service import (
    append_message,
    create_conversation,
    create_run,
    get_or_create_default_user,
    list_messages,
    resolve_default_model,
)
from app.core.db import engine
from app.core.logging import get_logger
from app.models.subagent import (
    SUBAGENT_STATUS_CANCELLED,
    SUBAGENT_STATUS_COMPLETED,
    SUBAGENT_STATUS_FAILED,
    SUBAGENT_STATUS_QUEUED,
    SUBAGENT_STATUS_RUNNING,
    TERMINAL_SUBAGENT_STATUSES,
    SubagentRole,
    SubagentRun,
)
from app.providers import get_provider_for_model

log = get_logger(__name__)


class SubagentRegistry:
    """In-memory registry tracking active subagent asyncio tasks.

    Enables concurrent execution and cancellation of subagent runs without
    polling the database for liveness.
    """

    def __init__(self) -> None:
        self._tasks: dict[int, asyncio.Task] = {}

    def register(self, subagent_run_id: int, task: asyncio.Task) -> None:
        self._tasks[subagent_run_id] = task

    def unregister(self, subagent_run_id: int) -> None:
        self._tasks.pop(subagent_run_id, None)

    def cancel(self, subagent_run_id: int) -> bool:
        task = self._tasks.get(subagent_run_id)
        if task is not None and not task.done():
            task.cancel()
            return True
        return False

    def is_active(self, subagent_run_id: int) -> bool:
        task = self._tasks.get(subagent_run_id)
        return task is not None and not task.done()

    @property
    def active_ids(self) -> list[int]:
        return [k for k, v in self._tasks.items() if not v.done()]


# Global singleton registry.
subagent_registry = SubagentRegistry()


def get_role(session: Session, role_id: int) -> SubagentRole | None:
    return session.get(SubagentRole, role_id)


def get_role_by_name(session: Session, name: str) -> SubagentRole | None:
    return session.exec(select(SubagentRole).where(SubagentRole.name == name)).first()


def list_roles(session: Session) -> Sequence[SubagentRole]:
    return session.exec(select(SubagentRole).order_by(SubagentRole.name)).all()


def create_role(
    session: Session,
    *,
    name: str,
    description: str | None = None,
    system_prompt: str | None = None,
    model: str | None = None,
    tool_names: list[str] | None = None,
    capability_policy: dict | None = None,
    max_iterations: int = 10,
    max_cost_usd: float | None = None,
    is_builtin: bool = False,
) -> SubagentRole:
    role = SubagentRole(
        name=name,
        description=description,
        system_prompt=system_prompt,
        model=model,
        tool_names=tool_names,
        capability_policy=capability_policy,
        max_iterations=max_iterations,
        max_cost_usd=max_cost_usd,
        is_builtin=is_builtin,
    )
    session.add(role)
    session.commit()
    session.refresh(role)
    return role


def update_role(session: Session, role_id: int, **fields) -> SubagentRole | None:
    role = session.get(SubagentRole, role_id)
    if role is None:
        return None
    for key, value in fields.items():
        if hasattr(role, key):
            setattr(role, key, value)
    session.add(role)
    session.commit()
    session.refresh(role)
    return role


def delete_role(session: Session, role_id: int) -> bool:
    role = session.get(SubagentRole, role_id)
    if role is None or role.is_builtin:
        return False
    session.delete(role)
    session.commit()
    return True


def get_subagent_run(session: Session, run_id: int) -> SubagentRun | None:
    return session.get(SubagentRun, run_id)


def list_subagent_runs(
    session: Session,
    *,
    parent_conversation_id: int | None = None,
    status: str | None = None,
) -> Sequence[SubagentRun]:
    stmt = select(SubagentRun).order_by(col(SubagentRun.id).desc())
    if parent_conversation_id is not None:
        stmt = stmt.where(SubagentRun.parent_conversation_id == parent_conversation_id)
    if status is not None:
        stmt = stmt.where(SubagentRun.status == status)
    return session.exec(stmt).all()


def create_subagent_run(
    session: Session,
    *,
    prompt: str,
    parent_conversation_id: int,
    role: SubagentRole | None = None,
    parent_run_id: int | None = None,
    name: str | None = None,
    model_override: str | None = None,
    profile_id: int | None = None,
    research_run_id: int | None = None,
) -> SubagentRun:
    """Create an isolated conversation + durable run + SubagentRun row.

    The subagent's conversation is completely separate from the parent,
    ensuring history isolation.
    """
    user = get_or_create_default_user(session)
    assert user.id is not None

    # Resolve profile config if profile_id is set (Фаза 3a §2).
    _profile = None
    if profile_id is not None:
        from app.agent.personalities.service import get_profile

        _profile = get_profile(session, profile_id)

    # Determine effective model (explicit override > profile > role > configured default).
    effective_model = (
        model_override
        or (_profile.model if _profile else None)
        or (role.model if role else None)
        or resolve_default_model(session)
    )

    # Inherit the parent conversation's working directory so file tools work.
    from app.models import Conversation

    parent_conv = session.get(Conversation, parent_conversation_id)
    working_directory = parent_conv.working_directory if parent_conv else None

    # Resolve capability policy: profile settings > role > None.
    _cap_policy = None
    if _profile and _profile.settings and isinstance(_profile.settings, dict):
        _cap_policy = _profile.settings.get("capability_policy")
    elif role:
        _cap_policy = role.capability_policy

    # Create isolated conversation for the subagent. Marked with is_subagent
    # metadata so it is hidden from the regular conversation list.
    display_name = name or (
        f"subagent:{_profile.name}"
        if _profile
        else f"subagent:{role.name}"
        if role
        else "subagent:adhoc"
    )
    conv = create_conversation(
        session,
        user_id=user.id,
        title=f"[Subagent] {display_name}",
        model=effective_model,
        working_directory=working_directory,
        capability_policy=_cap_policy,
    )
    conv.metadata_ = {**(conv.metadata_ or {}), "is_subagent": True}
    session.add(conv)
    session.commit()
    session.refresh(conv)
    assert conv.id is not None

    # Create the durable run row.
    run = create_run(
        session,
        conversation_id=conv.id,
        user_id=user.id,
        model=effective_model,
        status="queued",
    )

    # Persist the user prompt as the first message in the isolated conversation.
    append_message(session, conversation_id=conv.id, role="user", content=prompt)

    # Create the SubagentRun tracking row.
    subagent_run = SubagentRun(
        role_id=role.id if role else None,
        profile_id=profile_id,
        parent_conversation_id=parent_conversation_id,
        parent_run_id=parent_run_id,
        research_run_id=research_run_id,
        conversation_id=conv.id,
        run_id=run.id,
        name=display_name,
        prompt=prompt,
        status=SUBAGENT_STATUS_QUEUED,
    )
    session.add(subagent_run)
    session.commit()
    session.refresh(subagent_run)
    assert subagent_run.id is not None
    return subagent_run


async def execute_subagent(subagent_run_id: int) -> str | None:
    """Execute a subagent run to completion. Returns the result summary.

    This is designed to run as an asyncio task. It opens its own DB session,
    resolves the role config, and drives ``run_conversation_turn`` against
    the subagent's isolated conversation.
    """
    from app.agent import AgentLimits

    with Session(engine) as session:
        sa_run = session.get(SubagentRun, subagent_run_id)
        if sa_run is None:
            log.error("subagent.not_found", subagent_run_id=subagent_run_id)
            return None

        # Resolve role config.
        role = session.get(SubagentRole, sa_run.role_id) if sa_run.role_id else None

        # Resolve profile config (Фаза 3a §2 — cross-profile invocation).
        _profile = None
        if sa_run.profile_id:
            from app.agent.personalities.service import get_profile

            _profile = get_profile(session, sa_run.profile_id)

        # Effective settings from profile > role > defaults.
        system_prompt: str | None
        if _profile and _profile.system_prompt:
            system_prompt = _profile.system_prompt
        else:
            system_prompt = role.system_prompt if role else None
        # Get model from the run row.
        from app.agent.service import get_run

        run_row = get_run(session, sa_run.run_id) if sa_run.run_id else None
        effective_model = (
            (run_row.model if run_row else None)
            or (_profile.model if _profile else None)
            or (role.model if role else None)
            or resolve_default_model(session)
        )
        provider = get_provider_for_model(effective_model)
        from app.providers.registry import resolve_provider_model

        effective_model = resolve_provider_model(provider, effective_model)
        if effective_model is None:
            sa_run.status = SUBAGENT_STATUS_FAILED
            sa_run.error = "No default model is configured"
            session.add(sa_run)
            session.commit()
            return None

        tool_names = (
            _profile.tool_names
            if _profile and _profile.tool_names is not None
            else role.tool_names
            if role
            else None
        )
        max_iterations = role.max_iterations if role else 10
        max_cost_usd = role.max_cost_usd if role else None

        # Capability policy: profile settings > role's policy (already set on conversation).
        cap_policy = None
        if _profile and _profile.settings and isinstance(_profile.settings, dict):
            cap_policy = _profile.settings.get("capability_policy")
        if cap_policy is None:
            cap_policy = role.capability_policy if role else None

        limits = AgentLimits(
            max_iterations=max_iterations,
            max_cost_usd=max_cost_usd,
        )

        # Mark as running.
        sa_run.status = SUBAGENT_STATUS_RUNNING
        session.add(sa_run)
        session.commit()

        # Resolve working directory from the subagent's conversation.
        from app.models import Conversation

        sa_conv = session.get(Conversation, sa_run.conversation_id)
        working_directory = sa_conv.working_directory if sa_conv else None

        result_summary: str | None = None
        token_parts: list[str] = []  # Accumulate tokens as fallback.
        error_message: str | None = None  # Captured from "error" events.
        try:
            async for event in run_conversation_turn(
                session=session,
                conversation_id=sa_run.conversation_id,
                provider=provider,
                model=effective_model,
                user_input=None,  # Prompt already persisted in create_subagent_run.
                system_prompt=system_prompt,
                tool_names=tool_names,
                working_directory=working_directory,
                conversation_capability_policy=cap_policy,
                auto_approve=True,
                limits=limits,
                run_id=sa_run.run_id,
                cancellable=True,
            ):
                # Capture the final assistant content as the result summary.
                if event.kind == "token":
                    token_parts.append(event.payload.get("text", ""))
                elif event.kind == "message":
                    content = event.payload.get("content")
                    if content:
                        result_summary = content
                    # Reset token buffer for the next iteration.
                    token_parts.clear()
                elif event.kind == "error":
                    # The executor reports unrecoverable LLM failures as events
                    # (not exceptions). Capture so the run is marked failed and
                    # the parent agent gets a real error instead of empty output.
                    error_message = (
                        event.payload.get("detail")
                        or event.payload.get("message")
                        or "Unknown subagent error"
                    )
                elif event.kind == "finish":
                    usage = event.payload.get("usage")
                    if usage:
                        sa_run.usage = usage
                        session.add(sa_run)
                        session.commit()

            # Fallback: if no "message" event carried content, use accumulated tokens.
            if result_summary is None and token_parts:
                result_summary = "".join(token_parts).strip() or None

            # If the loop surfaced an error event, mark the run failed (not
            # completed) so the failure is visible instead of a silent no-output.
            if error_message is not None:
                sa_run.status = SUBAGENT_STATUS_FAILED
                sa_run.error = error_message
                sa_run.finished_at = datetime.now(UTC)
                session.add(sa_run)
                session.commit()
                log.error("subagent.failed", subagent_run_id=subagent_run_id, error=error_message)
                return None

            # Mark completed.
            sa_run.status = SUBAGENT_STATUS_COMPLETED
            sa_run.result_summary = result_summary
            sa_run.finished_at = datetime.now(UTC)
            session.add(sa_run)
            session.commit()
            log.info("subagent.completed", subagent_run_id=subagent_run_id)

        except asyncio.CancelledError:
            sa_run.status = SUBAGENT_STATUS_CANCELLED
            sa_run.finished_at = datetime.now(UTC)
            session.add(sa_run)
            session.commit()
            log.info("subagent.cancelled", subagent_run_id=subagent_run_id)
            raise

        except Exception as exc:
            sa_run.status = SUBAGENT_STATUS_FAILED
            sa_run.error = str(exc)
            sa_run.finished_at = datetime.now(UTC)
            session.add(sa_run)
            session.commit()
            log.error("subagent.failed", subagent_run_id=subagent_run_id, error=str(exc))

        finally:
            subagent_registry.unregister(subagent_run_id)

        return result_summary


def launch_subagent(
    session: Session,
    *,
    prompt: str,
    parent_conversation_id: int,
    role: SubagentRole | None = None,
    parent_run_id: int | None = None,
    name: str | None = None,
    model_override: str | None = None,
) -> SubagentRun:
    """Create and schedule a subagent run for async execution.

    Returns the SubagentRun row immediately (status=queued). The actual
    execution happens in a background asyncio task when an event loop is
    available.
    """
    sa_run = create_subagent_run(
        session,
        prompt=prompt,
        parent_conversation_id=parent_conversation_id,
        role=role,
        parent_run_id=parent_run_id,
        name=name,
        model_override=model_override,
    )
    assert sa_run.id is not None

    # Schedule execution as a background task if an event loop is running.
    try:
        loop = asyncio.get_running_loop()
        task = loop.create_task(execute_subagent(sa_run.id))
        subagent_registry.register(sa_run.id, task)
    except RuntimeError:
        # No running event loop (e.g. sync test client). The run stays queued
        # and can be executed explicitly via execute_subagent().
        log.debug("subagent.no_loop", subagent_run_id=sa_run.id)

    return sa_run


def cancel_subagent_run(session: Session, subagent_run_id: int) -> bool:
    """Cancel a running subagent. Returns True if cancellation was initiated."""
    sa_run = session.get(SubagentRun, subagent_run_id)
    if sa_run is None or sa_run.status in TERMINAL_SUBAGENT_STATUSES:
        return False

    # Cancel the asyncio task (which will set status via CancelledError handler).
    cancelled = subagent_registry.cancel(subagent_run_id)
    if not cancelled:
        # Task already done or not tracked; mark directly.
        sa_run.status = SUBAGENT_STATUS_CANCELLED
        sa_run.finished_at = datetime.now(UTC)
        session.add(sa_run)
        session.commit()
    return True


def delete_subagent_run(session: Session, subagent_run_id: int) -> bool:
    """Delete a terminal subagent run record."""
    sa_run = session.get(SubagentRun, subagent_run_id)
    if sa_run is None:
        return False
    if sa_run.status not in TERMINAL_SUBAGENT_STATUSES:
        return False
    session.delete(sa_run)
    session.commit()
    return True


def get_subagent_messages(session: Session, subagent_run_id: int) -> list:
    """Get all messages from a subagent's isolated conversation."""
    sa_run = session.get(SubagentRun, subagent_run_id)
    if sa_run is None:
        return []
    return list(list_messages(session, sa_run.conversation_id))


def ensure_builtin_roles(session: Session) -> None:
    """Seed built-in roles if they don't exist yet. Called on app startup."""
    builtins: list[dict[str, Any]] = [
        {
            "name": "researcher",
            "description": "Deep research agent that gathers and synthesizes information from multiple sources.",
            "system_prompt": (
                "You are a research specialist. Your job is to thoroughly investigate "
                "a topic, gather information from available sources, and produce a "
                "comprehensive, well-structured summary. Cite sources where possible."
            ),
            "tool_names": [
                "web_fetch",
                "web_search",
                "browser_navigate",
                "browser_click",
                "browser_extract",
                "browser_scroll",
                "browser_screenshot",
                "browser_close",
                "image_analyze",
                "read_file",
                "list_files",
            ],
            "capability_policy": {
                "read": "allow",
                "network": "allow",
                # Browser screenshots are durable Artifact writes; file-write
                # tools are not in this role's whitelist.
                "write": "allow",
                "execute": "deny",
            },
            "max_iterations": 15,
        },
        {
            "name": "code-reviewer",
            "description": "Code review agent that analyzes code for bugs, style issues, and improvements.",
            "system_prompt": (
                "You are a senior code reviewer. Analyze the provided code carefully, "
                "identifying bugs, security issues, performance problems, and style "
                "violations. Provide specific, actionable feedback with code examples."
            ),
            "tool_names": ["read_file", "list_files"],
            "capability_policy": {
                "read": "allow",
                "write": "deny",
                "execute": "deny",
                "network": "deny",
            },
            "max_iterations": 10,
        },
        {
            "name": "summarizer",
            "description": "Document summarization agent that produces concise, accurate summaries.",
            "system_prompt": (
                "You are a summarization expert. Read the provided content and produce "
                "a clear, concise summary that captures all key points. Structure your "
                "summary with headings and bullet points for readability."
            ),
            "tool_names": ["read_file", "list_files"],
            "capability_policy": {
                "read": "allow",
                "write": "deny",
                "execute": "deny",
                "network": "deny",
            },
            "max_iterations": 5,
        },
    ]
    for role_def in builtins:
        existing = session.exec(
            select(SubagentRole).where(SubagentRole.name == role_def["name"])
        ).first()
        if existing is None:
            create_role(session, is_builtin=True, **role_def)
        elif (
            existing.is_builtin
            and existing.name == "researcher"
            and existing.tool_names == ["web_fetch", "web_search", "read_file", "list_files"]
        ):
            # One-time forward migration for the legacy built-in role. Custom
            # user edits are preserved because only the exact old list matches.
            existing.tool_names = role_def["tool_names"]
            existing.capability_policy = role_def["capability_policy"]
            session.add(existing)
            session.commit()
