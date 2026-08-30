"""Agent Constructor services: blueprint helpers and composable macro tools."""

from __future__ import annotations

import re
import time
from copy import deepcopy
from typing import Any

from jsonschema import SchemaError
from jsonschema.validators import Draft202012Validator
from pydantic import ConfigDict
from sqlmodel import Session, select

from app.agent.permissions import PermissionsConfig
from app.agent.service import append_run_events, update_run
from app.core.config import get_settings
from app.core.db import engine
from app.models import ApprovalAudit, ToolCall
from app.models.macro_tool import MacroTool
from app.security.capabilities import Capability, stricter
from app.security.secrets import mask_secrets_in_value
from app.tools.base import ToolArgs, ToolResult, get_registry, get_tool, register_tool
from app.tools.context import get_run_context

_NAME_RE = re.compile(r"^[a-z][a-z0-9_]{2,63}$")
_REF_RE = re.compile(r"\$\{(input\.[A-Za-z0-9_.-]+|steps\.[A-Za-z0-9_-]+\.output)\}")


class _MacroArgs(ToolArgs):
    model_config = ConfigDict(extra="allow")


def validate_macro(*, name: str, input_schema: dict[str, Any], steps: list[dict[str, Any]]) -> None:
    """Reject malformed, recursive, or unknown macro definitions."""
    if not _NAME_RE.fullmatch(name):
        raise ValueError("Macro name must be snake_case (3-64 characters)")
    if not name.startswith("macro_"):
        raise ValueError("Macro name must start with 'macro_'")
    if name in get_registry() and not get_registry()[name].name.startswith("macro_"):
        raise ValueError(f"Tool name '{name}' is already registered")
    if input_schema.get("type") != "object" or not isinstance(
        input_schema.get("properties", {}), dict
    ):
        raise ValueError("input_schema must be a JSON Schema object")
    try:
        Draft202012Validator.check_schema(input_schema)
    except SchemaError as exc:
        raise ValueError(f"input_schema is not valid JSON Schema: {exc.message}") from exc
    if not steps or len(steps) > 20:
        raise ValueError("A macro must contain 1-20 steps")

    seen: set[str] = set()
    registry = get_registry()
    for index, step in enumerate(steps):
        step_id = str(step.get("id") or "")
        tool_name = str(step.get("tool_name") or "")
        arguments = step.get("arguments", {})
        if not _NAME_RE.fullmatch(step_id):
            raise ValueError(f"Step {index + 1} has an invalid id")
        if step_id in seen:
            raise ValueError(f"Duplicate step id '{step_id}'")
        if tool_name == name or tool_name.startswith("macro_"):
            raise ValueError("Macros may compose base tools only (no recursion)")
        if tool_name not in registry:
            raise ValueError(f"Unknown tool '{tool_name}'")
        if not isinstance(arguments, dict):
            raise ValueError(f"Step '{step_id}' arguments must be an object")
        # A step can only reference outputs produced earlier in the sequence.
        for ref in _iter_refs(arguments):
            if ref.startswith("steps.") and ref.split(".")[1] not in seen:
                raise ValueError(f"Step '{step_id}' references a future or unknown step: {ref}")
        seen.add(step_id)


def list_macros(session: Session) -> list[MacroTool]:
    return list(session.exec(select(MacroTool).order_by(MacroTool.name)).all())


def create_macro(
    session: Session,
    *,
    name: str,
    description: str,
    input_schema: dict[str, Any],
    steps: list[dict[str, Any]],
) -> MacroTool:
    validate_macro(name=name, input_schema=input_schema, steps=steps)
    if session.exec(select(MacroTool).where(MacroTool.name == name)).first():
        raise ValueError(f"Macro '{name}' already exists")
    macro = MacroTool(
        name=name,
        description=description,
        input_schema=deepcopy(input_schema),
        steps=deepcopy(steps),
    )
    session.add(macro)
    session.commit()
    session.refresh(macro)
    register_macro(macro)
    return macro


def update_macro(
    session: Session,
    macro_id: int,
    *,
    description: str | None = None,
    input_schema: dict[str, Any] | None = None,
    steps: list[dict[str, Any]] | None = None,
    is_active: bool | None = None,
) -> MacroTool | None:
    macro = session.get(MacroTool, macro_id)
    if macro is None:
        return None
    schema = input_schema if input_schema is not None else macro.input_schema
    resolved_steps = steps if steps is not None else macro.steps
    validate_macro(name=macro.name, input_schema=schema, steps=resolved_steps)
    if description is not None:
        macro.description = description
    if input_schema is not None:
        macro.input_schema = deepcopy(input_schema)
    if steps is not None:
        macro.steps = deepcopy(steps)
    if is_active is not None:
        macro.is_active = is_active
    session.add(macro)
    session.commit()
    session.refresh(macro)
    if macro.is_active:
        register_macro(macro)
    else:
        get_registry().pop(macro.name, None)
    return macro


def delete_macro(session: Session, macro_id: int) -> bool:
    macro = session.get(MacroTool, macro_id)
    if macro is None:
        return False
    get_registry().pop(macro.name, None)
    session.delete(macro)
    session.commit()
    return True


def load_macro_tools(session: Session) -> int:
    macros = session.exec(select(MacroTool).where(MacroTool.is_active == True)).all()  # noqa: E712
    for macro in macros:
        try:
            validate_macro(name=macro.name, input_schema=macro.input_schema, steps=macro.steps)
            register_macro(macro)
        except ValueError:
            continue
    return len(macros)


def register_macro(macro: MacroTool) -> None:
    """Register a DB-backed macro in the normal pluggable tool registry."""
    capabilities: set[Capability] = set()
    dangerous = False
    for step in macro.steps:
        tool = get_tool(step["tool_name"])
        if tool is not None:
            capabilities.update(tool.capabilities or ())
            dangerous = dangerous or tool.dangerous

    async def _run_macro(**inputs: Any) -> ToolResult:
        outputs: dict[str, str] = {}
        llm_usage: dict[str, Any] = {
            "prompt_tokens": 0,
            "completion_tokens": 0,
            "total_tokens": 0,
            "cost_usd": None,
        }
        llm_model: str | None = None
        llm_provider: str | None = None
        llm_duration_ms = 0
        ctx = get_run_context()
        permissions = PermissionsConfig(ctx.permissions)
        for step in macro.steps:
            tool = get_tool(step["tool_name"])
            if tool is None:
                return ToolResult.err(f"Macro step tool '{step['tool_name']}' is unavailable")
            tool_decision = permissions.resolve(tool.name, dangerous=tool.dangerous)
            cap_decision = (
                ctx.capability_policy.resolve_tool(tool.name)
                if ctx.capability_policy is not None
                else "allow"
            )
            decision = stricter(tool_decision, cap_decision)
            # An outer macro call is preflighted by AgentExecutor across every
            # composed tool. ``ask`` has therefore already been approved once.
            if decision == "deny" or (
                decision == "ask" and tool.name not in ctx.approved_composed_tools
            ):
                return ToolResult.err(
                    f"Macro step '{step['id']}' requires {decision} permission for {tool.name}"
                )
            arguments = _expand(step.get("arguments", {}), inputs, outputs)
            call_id = f"macro:{macro.id}:{step['id']}"
            started_at = time.monotonic()
            _record_macro_step_start(
                ctx=ctx,
                call_id=call_id,
                macro=macro,
                step=step,
                arguments=arguments,
            )
            result = await tool.run(arguments)
            step_metadata = result.metadata or {}
            step_usage = step_metadata.get("llm_usage")
            if isinstance(step_usage, dict):
                for key in ("prompt_tokens", "completion_tokens", "total_tokens"):
                    llm_usage[key] = int(llm_usage[key] or 0) + int(step_usage.get(key, 0) or 0)
                if step_usage.get("cost_usd") is not None:
                    llm_usage["cost_usd"] = float(llm_usage["cost_usd"] or 0.0) + float(
                        step_usage["cost_usd"]
                    )
                llm_model = str(step_metadata.get("llm_model") or llm_model or "") or None
                llm_provider = (
                    str(step_metadata.get("llm_provider") or llm_provider or "") or None
                )
                llm_duration_ms += int(step_metadata.get("llm_duration_ms") or 0)
            duration_ms = int((time.monotonic() - started_at) * 1000)
            _record_macro_step_result(
                ctx=ctx,
                call_id=call_id,
                macro=macro,
                step=step,
                arguments=arguments,
                result=result,
                duration_ms=duration_ms,
            )
            outputs[step["id"]] = result.output
            if result.is_error:
                return ToolResult.err(
                    f"Macro stopped at step '{step['id']}': {result.error or result.output}",
                    step_id=step["id"],
                    completed_steps=list(outputs),
                    llm_usage=llm_usage if llm_model else None,
                    llm_model=llm_model,
                    llm_provider=llm_provider,
                    llm_duration_ms=llm_duration_ms,
                )
        return ToolResult.ok(
            outputs[next(reversed(outputs))],
            macro_id=macro.id,
            step_outputs=outputs,
            llm_usage=llm_usage if llm_model else None,
            llm_model=llm_model,
            llm_provider=llm_provider,
            llm_duration_ms=llm_duration_ms,
        )

    register_tool(
        name=macro.name,
        description=macro.description or f"Run the {macro.name} macro",
        args_model=_MacroArgs,
        func=_run_macro,
        dangerous=dangerous,
        capabilities=frozenset(capabilities),
        parameters=macro.input_schema,
        composed_tools=tuple(str(step["tool_name"]) for step in macro.steps),
    )


def _record_macro_step_start(
    *,
    ctx,
    call_id: str,
    macro: MacroTool,
    step: dict[str, Any],
    arguments: dict[str, Any],
) -> None:
    if ctx.run_id is None:
        return
    with Session(engine) as session:
        append_run_events(
            session,
            run_id=ctx.run_id,
            events=[
                (
                    "tool_call_start",
                    {
                        "id": call_id,
                        "name": step["tool_name"],
                        "arguments": mask_secrets_in_value(
                            arguments, enabled=get_settings().mask_secrets
                        ),
                        "macro_id": macro.id,
                        "macro_name": macro.name,
                        "macro_step_id": step["id"],
                    },
                )
            ],
        )


def _record_macro_step_result(
    *,
    ctx,
    call_id: str,
    macro: MacroTool,
    step: dict[str, Any],
    arguments: dict[str, Any],
    result: ToolResult,
    duration_ms: int,
) -> None:
    if ctx.run_id is None or ctx.conversation_id is None:
        return
    settings = get_settings()
    masked_arguments = mask_secrets_in_value(arguments, enabled=settings.mask_secrets)
    masked_output = mask_secrets_in_value(result.output, enabled=settings.mask_secrets)
    payload = {
        "id": call_id,
        "name": step["tool_name"],
        "result": {
            "output": masked_output,
            "error": result.error,
            "is_error": result.is_error,
            "metadata": {
                **(result.metadata or {}),
                "duration_ms": duration_ms,
                "macro_id": macro.id,
                "macro_step_id": step["id"],
            },
        },
    }
    with Session(engine) as session:
        append_run_events(
            session,
            run_id=ctx.run_id,
            events=[("tool_result", payload)],
        )
        session.add(
            ToolCall(
                conversation_id=ctx.conversation_id,
                name=str(step["tool_name"]),
                arguments=masked_arguments,
                result=payload["result"],
                duration_ms=duration_ms,
                success=not result.is_error,
                error=result.error,
            )
        )
        session.add(
            ApprovalAudit(
                conversation_id=ctx.conversation_id,
                run_id=ctx.run_id,
                call_id=call_id,
                tool_name=str(step["tool_name"]),
                arguments=masked_arguments,
                approved=True,
                decision_source="macro_preflight",
                decided_by="agent_executor",
                reason=f"Approved as composed step of {macro.name}",
                duration_ms=duration_ms,
            )
        )
        update_run(
            session,
            ctx.run_id,
            commit=False,
            checkpoint={
                "macro_id": macro.id,
                "macro_name": macro.name,
                "macro_step_id": step["id"],
                "last_call_id": call_id,
                "last_tool": step["tool_name"],
                "success": not result.is_error,
            },
        )
        session.commit()


def _iter_refs(value: Any):
    if isinstance(value, str):
        yield from (match.group(1) for match in _REF_RE.finditer(value))
    elif isinstance(value, dict):
        for nested in value.values():
            yield from _iter_refs(nested)
    elif isinstance(value, list):
        for nested in value:
            yield from _iter_refs(nested)


def _expand(value: Any, inputs: dict[str, Any], outputs: dict[str, str]) -> Any:
    if isinstance(value, dict):
        return {key: _expand(nested, inputs, outputs) for key, nested in value.items()}
    if isinstance(value, list):
        return [_expand(nested, inputs, outputs) for nested in value]
    if not isinstance(value, str):
        return value

    def replace(match: re.Match[str]) -> str:
        ref = match.group(1)
        if ref.startswith("input."):
            resolved: Any = inputs
            for part in ref.split(".")[1:]:
                resolved = resolved.get(part, "") if isinstance(resolved, dict) else ""
            return str(resolved)
        return outputs.get(ref.split(".")[1], "")

    return _REF_RE.sub(replace, value)
