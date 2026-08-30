"""CRUD service for agent profiles (Фаза 3a §2)."""

from __future__ import annotations

from collections.abc import Sequence

from sqlmodel import Session, select

from app.models.profile import AgentProfile


def get_profile(session: Session, profile_id: int) -> AgentProfile | None:
    return session.get(AgentProfile, profile_id)


def get_profile_by_slug(session: Session, slug: str) -> AgentProfile | None:
    return session.exec(select(AgentProfile).where(AgentProfile.slug == slug)).first()


def list_profiles(session: Session, *, include_inactive: bool = False) -> Sequence[AgentProfile]:
    stmt = select(AgentProfile).order_by(AgentProfile.name)
    if not include_inactive:
        stmt = stmt.where(AgentProfile.is_active == True)  # noqa: E712
    return session.exec(stmt).all()


def create_profile(
    session: Session,
    *,
    name: str,
    slug: str,
    description: str | None = None,
    system_prompt: str | None = None,
    model: str | None = None,
    tool_names: list[str] | None = None,
    skill_names: list[str] | None = None,
    settings: dict | None = None,
    avatar_color: str | None = None,
    is_builtin: bool = False,
    is_active: bool = True,
    is_shared: bool = False,
) -> AgentProfile:
    profile = AgentProfile(
        name=name,
        slug=slug,
        description=description,
        system_prompt=system_prompt,
        model=model,
        tool_names=tool_names,
        skill_names=skill_names,
        settings=settings,
        avatar_color=avatar_color,
        is_builtin=is_builtin,
        is_active=is_active,
        is_shared=is_shared,
    )
    session.add(profile)
    session.commit()
    session.refresh(profile)
    return profile


def update_profile(
    session: Session,
    profile_id: int,
    *,
    name: str | None = None,
    slug: str | None = None,
    description: str | None = None,
    system_prompt: str | None = None,
    model: str | None = None,
    tool_names: list[str] | None = None,
    skill_names: list[str] | None = None,
    settings: dict | None = None,
    avatar_color: str | None = None,
    is_active: bool | None = None,
    is_shared: bool | None = None,
) -> AgentProfile | None:
    """Patch updatable fields. None means 'leave unchanged'."""
    profile = session.get(AgentProfile, profile_id)
    if profile is None:
        return None
    if name is not None:
        profile.name = name
    if slug is not None:
        profile.slug = slug
    if description is not None:
        profile.description = description
    if system_prompt is not None:
        profile.system_prompt = system_prompt
    if model is not None:
        profile.model = model or None
    if tool_names is not None:
        # [] is an explicit "no tools" whitelist; None means inherit/all.
        profile.tool_names = list(tool_names)
    if skill_names is not None:
        profile.skill_names = list(skill_names)
    if settings is not None:
        profile.settings = settings or None
    if avatar_color is not None:
        profile.avatar_color = avatar_color
    if is_active is not None:
        profile.is_active = is_active
    if is_shared is not None:
        profile.is_shared = is_shared
    session.add(profile)
    session.commit()
    session.refresh(profile)
    return profile


def delete_profile(session: Session, profile_id: int) -> bool:
    """Delete a profile. Returns False if not found. Raises ValueError for builtins."""
    profile = session.get(AgentProfile, profile_id)
    if profile is None:
        return False
    if profile.is_builtin:
        raise ValueError("Cannot delete a built-in profile")
    session.delete(profile)
    session.commit()
    return True
