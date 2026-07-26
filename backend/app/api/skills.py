"""Skills API endpoints (Фаза 2 §3).

Provides REST endpoints for listing available skills and creating new ones.
The frontend settings page and project settings dialog consume these.
"""

from __future__ import annotations

import re
from pathlib import Path

from fastapi import APIRouter, HTTPException
from pydantic import BaseModel, Field

from app.core.config import get_settings
from app.core.logging import get_logger
from app.skills.registry import get_skill_registry

log = get_logger(__name__)

router = APIRouter()

# --- Schemas ---


class SkillOut(BaseModel):
    """A single skill as returned by the API."""

    name: str
    description: str
    source: str
    tags: list[str] = []
    tools: list[str] = []
    version: str = "1.0"
    body: str = ""


class SkillListResponse(BaseModel):
    skills: list[SkillOut]


class SkillCreateRequest(BaseModel):
    """Request body for creating a new skill."""

    name: str = Field(..., min_length=1, max_length=64, description="Skill name (directory name)")
    description: str = Field(default="", max_length=500)
    tags: list[str] = Field(default_factory=list)
    tools: list[str] = Field(default_factory=list)
    body: str = Field(..., min_length=1, description="Skill instruction content (markdown)")
    scope: str = Field(
        default="global",
        description="Where to create: 'global' (skills/) or 'user' (data/skills/)",
    )


class SkillCreateResponse(BaseModel):
    name: str
    path: str
    scope: str


# --- Validation ---

_SKILL_NAME_RE = re.compile(r"^[a-z0-9]+(?:-[a-z0-9]+)*$")


def _validate_skill_name(name: str) -> None:
    """Validate a skill name: lowercase alphanumeric with hyphens."""
    if not _SKILL_NAME_RE.match(name):
        raise HTTPException(
            status_code=422,
            detail=(
                f"Invalid skill name '{name}'. Must be lowercase alphanumeric "
                "with hyphens (e.g. 'my-skill')."
            ),
        )


# --- Endpoints ---


@router.get("/skills", response_model=SkillListResponse)
async def list_skills(source: str | None = None) -> SkillListResponse:
    """List all available skills, optionally filtered by source."""
    registry = get_skill_registry()
    skills = registry.list_by_source(source) if source else registry.list_all()

    return SkillListResponse(
        skills=[
            SkillOut(
                name=s.name,
                description=s.description,
                source=s.source,
                tags=s.tags,
                tools=s.tools,
                version=s.version,
                body=s.body,
            )
            for s in skills
        ]
    )


@router.post("/skills", response_model=SkillCreateResponse, status_code=201)
async def create_skill(req: SkillCreateRequest) -> SkillCreateResponse:
    """Create a new skill by writing a SKILL.md file to the appropriate directory."""
    _validate_skill_name(req.name)

    if req.scope not in ("global", "user"):
        raise HTTPException(status_code=422, detail="scope must be 'global' or 'user'")

    settings = get_settings()

    # Determine target directory.
    if req.scope == "global":
        base_dir = Path(settings.skills_dir)
    else:
        base_dir = Path(settings.data_dir) / "skills"

    skill_dir = base_dir / req.name
    if skill_dir.exists():
        raise HTTPException(status_code=409, detail=f"Skill '{req.name}' already exists.")

    # Build SKILL.md content.
    lines = ["---"]
    lines.append(f"name: {req.name}")
    if req.description:
        lines.append(f"description: {req.description}")
    lines.append('version: "1.0"')
    if req.tags:
        lines.append("tags:")
        for tag in req.tags:
            lines.append(f"  - {tag}")
    if req.tools:
        lines.append("tools:")
        for tool in req.tools:
            lines.append(f"  - {tool}")
    lines.append("---")
    lines.append("")
    lines.append(req.body)
    lines.append("")

    content = "\n".join(lines)

    try:
        skill_dir.mkdir(parents=True, exist_ok=True)
        (skill_dir / "SKILL.md").write_text(content, encoding="utf-8")
    except OSError as exc:
        raise HTTPException(status_code=500, detail=f"Failed to create skill: {exc}") from exc

    # Reload the registry so the new skill is immediately available.
    registry = get_skill_registry()
    registry.load(force=True)

    log.info("skill.created", name=req.name, scope=req.scope, path=str(skill_dir))

    return SkillCreateResponse(name=req.name, path=str(skill_dir), scope=req.scope)


@router.delete("/skills/{name}", status_code=204)
async def delete_skill(name: str) -> None:
    """Delete a user-created skill (builtin skills cannot be deleted)."""
    _validate_skill_name(name)

    registry = get_skill_registry()
    skill = registry.get(name)
    if skill is None:
        raise HTTPException(status_code=404, detail=f"Skill '{name}' not found.")
    if skill.source == "builtin":
        raise HTTPException(status_code=403, detail="Cannot delete builtin skills.")

    import shutil

    try:
        shutil.rmtree(skill.path)
    except OSError as exc:
        raise HTTPException(status_code=500, detail=f"Failed to delete skill: {exc}") from exc

    registry.load(force=True)
    log.info("skill.deleted", name=name)
