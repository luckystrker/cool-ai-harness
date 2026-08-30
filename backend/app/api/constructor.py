"""Agent Constructor API: tool catalog and validated macro-tool CRUD."""

from __future__ import annotations

from fastapi import APIRouter, Depends, HTTPException
from sqlmodel import Session

from app.agent.constructor import create_macro, delete_macro, list_macros, update_macro
from app.api.schemas import MacroToolCreate, MacroToolOut, MacroToolUpdate, ToolCatalogItem
from app.core.db import get_session
from app.tools import get_registry

router = APIRouter(prefix="/agent-constructor", tags=["agent-constructor"])


def _macro_out(macro) -> MacroToolOut:
    return MacroToolOut(
        id=macro.id,
        name=macro.name,
        description=macro.description,
        input_schema=macro.input_schema,
        steps=macro.steps,
        is_active=macro.is_active,
        created_at=macro.created_at,
        updated_at=macro.updated_at,
    )


@router.get("/tools", response_model=list[ToolCatalogItem])
def tool_catalog() -> list[ToolCatalogItem]:
    return [
        ToolCatalogItem(
            name=tool.name,
            description=tool.description,
            dangerous=tool.dangerous,
            capabilities=sorted(cap.value for cap in (tool.capabilities or ())),
            parameters=tool.parameters_schema(),
            is_macro=tool.name.startswith("macro_"),
        )
        for tool in sorted(get_registry().values(), key=lambda item: item.name)
    ]


@router.get("/macros", response_model=list[MacroToolOut])
def get_macros(session: Session = Depends(get_session)) -> list[MacroToolOut]:
    return [_macro_out(macro) for macro in list_macros(session)]


@router.post("/macros", response_model=MacroToolOut, status_code=201)
def post_macro(body: MacroToolCreate, session: Session = Depends(get_session)) -> MacroToolOut:
    try:
        macro = create_macro(
            session,
            name=body.name,
            description=body.description,
            input_schema=body.input_schema,
            steps=body.steps,
        )
    except ValueError as exc:
        raise HTTPException(status_code=422, detail=str(exc)) from exc
    return _macro_out(macro)


@router.patch("/macros/{macro_id}", response_model=MacroToolOut)
def patch_macro(
    macro_id: int,
    body: MacroToolUpdate,
    session: Session = Depends(get_session),
) -> MacroToolOut:
    try:
        macro = update_macro(
            session,
            macro_id,
            description=body.description,
            input_schema=body.input_schema,
            steps=body.steps,
            is_active=body.is_active,
        )
    except ValueError as exc:
        raise HTTPException(status_code=422, detail=str(exc)) from exc
    if macro is None:
        raise HTTPException(status_code=404, detail="Macro not found")
    return _macro_out(macro)


@router.delete("/macros/{macro_id}")
def delete_macro_route(macro_id: int, session: Session = Depends(get_session)) -> dict:
    if not delete_macro(session, macro_id):
        raise HTTPException(status_code=404, detail="Macro not found")
    return {"deleted": macro_id}
