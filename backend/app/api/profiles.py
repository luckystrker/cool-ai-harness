"""Agent profile routes: CRUD + seeding (Фаза 3a §2 — Multi-personality agents)."""

from __future__ import annotations

from fastapi import APIRouter, Depends, HTTPException, Query
from sqlmodel import Session

from app.agent.personalities.seeding import seed_builtin_profiles
from app.agent.personalities.service import (
    create_profile,
    delete_profile,
    get_profile,
    list_profiles,
    update_profile,
)
from app.api.schemas import BlueprintPlaygroundRequest, ProfileCreate, ProfileOut, ProfileUpdate
from app.core.db import get_session

router = APIRouter(prefix="/profiles", tags=["profiles"])


def _profile_to_out(p) -> ProfileOut:
    return ProfileOut(
        id=p.id,
        name=p.name,
        slug=p.slug,
        description=p.description,
        system_prompt=p.system_prompt,
        model=p.model,
        tool_names=p.tool_names,
        skill_names=p.skill_names,
        settings=p.settings,
        avatar_color=p.avatar_color,
        is_builtin=p.is_builtin,
        is_active=p.is_active,
        is_shared=p.is_shared,
        created_at=p.created_at,
        updated_at=p.updated_at,
    )


@router.get("", response_model=list[ProfileOut])
def get_profiles(
    include_inactive: bool = Query(default=False),
    session: Session = Depends(get_session),
) -> list[ProfileOut]:
    profiles = list_profiles(session, include_inactive=include_inactive)
    return [_profile_to_out(p) for p in profiles]


@router.get("/{profile_id}", response_model=ProfileOut)
def get_profile_detail(profile_id: int, session: Session = Depends(get_session)) -> ProfileOut:
    profile = get_profile(session, profile_id)
    if profile is None:
        raise HTTPException(status_code=404, detail="Profile not found")
    return _profile_to_out(profile)


@router.post("", response_model=ProfileOut, status_code=201)
def post_profile(body: ProfileCreate, session: Session = Depends(get_session)) -> ProfileOut:
    from app.agent.personalities.service import get_profile_by_slug

    if get_profile_by_slug(session, body.slug) is not None:
        raise HTTPException(status_code=409, detail=f"Slug '{body.slug}' already exists")
    profile = create_profile(
        session,
        name=body.name,
        slug=body.slug,
        description=body.description,
        system_prompt=body.system_prompt,
        model=body.model,
        tool_names=body.tool_names,
        skill_names=body.skill_names,
        settings=body.settings,
        avatar_color=body.avatar_color,
        is_shared=body.is_shared,
    )
    return _profile_to_out(profile)


@router.patch("/{profile_id}", response_model=ProfileOut)
def patch_profile(
    profile_id: int,
    body: ProfileUpdate,
    session: Session = Depends(get_session),
) -> ProfileOut:
    profile = update_profile(
        session,
        profile_id,
        name=body.name,
        slug=body.slug,
        description=body.description,
        system_prompt=body.system_prompt,
        model=body.model,
        tool_names=body.tool_names,
        skill_names=body.skill_names,
        settings=body.settings,
        avatar_color=body.avatar_color,
        is_active=body.is_active,
        is_shared=body.is_shared,
    )
    if profile is None:
        raise HTTPException(status_code=404, detail="Profile not found")
    return _profile_to_out(profile)


@router.delete("/{profile_id}")
def delete_profile_route(profile_id: int, session: Session = Depends(get_session)) -> dict:
    try:
        if not delete_profile(session, profile_id):
            raise HTTPException(status_code=404, detail="Profile not found")
    except ValueError as exc:
        raise HTTPException(status_code=403, detail=str(exc)) from exc
    return {"deleted": profile_id}


@router.post("/seed")
def post_seed_profiles(session: Session = Depends(get_session)) -> dict:
    """Re-seed built-in presets (idempotent)."""
    created = seed_builtin_profiles(session)
    return {"created": created}


@router.post("/{profile_id}/clone", response_model=ProfileOut, status_code=201)
def clone_profile_route(profile_id: int, session: Session = Depends(get_session)) -> ProfileOut:
    """Copy a blueprint so it can be customized or shared independently."""
    source = get_profile(session, profile_id)
    if source is None:
        raise HTTPException(status_code=404, detail="Profile not found")
    base_slug = f"{source.slug}-copy"
    slug = base_slug
    suffix = 2
    from app.agent.personalities.service import get_profile_by_slug

    while get_profile_by_slug(session, slug) is not None:
        slug = f"{base_slug}-{suffix}"
        suffix += 1
    cloned = create_profile(
        session,
        name=f"{source.name} Copy",
        slug=slug,
        description=source.description,
        system_prompt=source.system_prompt,
        model=source.model,
        tool_names=list(source.tool_names) if source.tool_names is not None else None,
        skill_names=list(source.skill_names) if source.skill_names is not None else None,
        settings=dict(source.settings) if source.settings else None,
        avatar_color=source.avatar_color,
        is_shared=False,
    )
    return _profile_to_out(cloned)


@router.post("/{profile_id}/playground", status_code=201)
def launch_profile_playground(
    profile_id: int,
    body: BlueprintPlaygroundRequest,
    session: Session = Depends(get_session),
) -> dict:
    """Create a normal chat preconfigured with the selected blueprint."""
    profile = get_profile(session, profile_id)
    if profile is None or not profile.is_active:
        raise HTTPException(status_code=404, detail="Active profile not found")
    from app.agent.service import append_message, create_conversation, get_or_create_default_user

    user = get_or_create_default_user(session)
    assert user.id is not None
    conv = create_conversation(
        session,
        user_id=user.id,
        title=body.title or f"{profile.name} playground",
        model=profile.model,
        profile_id=profile.id,
    )
    assert conv.id is not None
    if body.initial_prompt:
        append_message(
            session,
            conversation_id=conv.id,
            role="user",
            content=body.initial_prompt,
        )
    return {"conversation_id": conv.id}
