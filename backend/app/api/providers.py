"""Providers API: CRUD for stored LLM provider credentials.

Keys are encrypted at rest via Fernet (app.core.security). The list/get
endpoints never return the decrypted secret — only a masked hint.
"""

from __future__ import annotations

from fastapi import APIRouter, Depends, HTTPException
from pydantic import BaseModel, Field
from sqlmodel import Session, select

from app.core.db import get_session
from app.core.logging import get_logger
from app.core.security import decrypt, encrypt
from app.models import Provider

log = get_logger(__name__)

router = APIRouter()


# --- schemas ---


class ProviderCreate(BaseModel):
    name: str = Field(..., description="openai | openrouter | deepseek | groq | ollama | anthropic | subscription/*")
    label: str | None = None
    base_url: str | None = None
    api_key: str  # plaintext from the client; stored encrypted
    default_model: str | None = None
    is_subscription: bool = False
    is_fallback: bool = Field(default=False, description="Use as the backup provider when the primary is unhealthy (Фаза 1.5 §5)")


class ProviderUpdate(BaseModel):
    label: str | None = None
    base_url: str | None = None
    api_key: str | None = None  # if provided, replaces the stored secret
    default_model: str | None = None
    is_active: bool | None = None
    is_fallback: bool | None = None


class ProviderOut(BaseModel):
    id: int
    name: str
    label: str | None
    base_url: str | None
    default_model: str | None
    is_active: bool
    is_subscription: bool
    is_fallback: bool = False
    # Masked preview of the stored key, e.g. "sk-…1a2b". Never the full secret.
    api_key_hint: str | None = None


def _mask(secret: str) -> str:
    if len(secret) <= 8:
        return "…"
    return f"{secret[:3]}…{secret[-4:]}"


def _to_out(p: Provider) -> ProviderOut:
    hint = None
    if p.api_key_encrypted:
        try:
            hint = _mask(decrypt(p.api_key_encrypted))
        except ValueError:
            hint = "<undecryptable>"
    return ProviderOut(
        id=p.id,
        name=p.name,
        label=p.label,
        base_url=p.base_url,
        default_model=p.default_model,
        is_active=p.is_active,
        is_subscription=p.is_subscription,
        is_fallback=p.is_fallback,
        api_key_hint=hint,
    )


# --- routes ---


@router.post("/providers", response_model=ProviderOut)
def create_provider(
    body: ProviderCreate,
    session: Session = Depends(get_session),
) -> ProviderOut:
    # MVP single-user: everything belongs to user_id=1.
    user_id = 1
    provider = Provider(
        user_id=user_id,
        name=body.name,
        label=body.label,
        base_url=body.base_url,
        api_key_encrypted=encrypt(body.api_key),
        default_model=body.default_model,
        is_subscription=body.is_subscription,
        is_fallback=body.is_fallback,
    )
    session.add(provider)
    session.commit()
    session.refresh(provider)
    log.info("providers.created", id=provider.id, name=provider.name)
    return _to_out(provider)


@router.get("/providers", response_model=list[ProviderOut])
def list_providers(session: Session = Depends(get_session)) -> list[ProviderOut]:
    rows = session.exec(select(Provider).where(Provider.user_id == 1)).all()
    return [_to_out(p) for p in rows]


@router.get("/providers/{provider_id}", response_model=ProviderOut)
def get_provider(
    provider_id: int, session: Session = Depends(get_session)
) -> ProviderOut:
    p = session.get(Provider, provider_id)
    if p is None:
        raise HTTPException(status_code=404, detail="Provider not found")
    return _to_out(p)


@router.patch("/providers/{provider_id}", response_model=ProviderOut)
def update_provider(
    provider_id: int,
    body: ProviderUpdate,
    session: Session = Depends(get_session),
) -> ProviderOut:
    p = session.get(Provider, provider_id)
    if p is None:
        raise HTTPException(status_code=404, detail="Provider not found")
    if body.label is not None:
        p.label = body.label
    if body.base_url is not None:
        p.base_url = body.base_url
    if body.default_model is not None:
        p.default_model = body.default_model
    if body.is_active is not None:
        p.is_active = body.is_active
    if body.is_fallback is not None:
        p.is_fallback = body.is_fallback
    if body.api_key is not None:
        p.api_key_encrypted = encrypt(body.api_key)
    session.add(p)
    session.commit()
    session.refresh(p)
    return _to_out(p)


@router.delete("/providers/{provider_id}")
def delete_provider(
    provider_id: int, session: Session = Depends(get_session)
) -> dict:
    p = session.get(Provider, provider_id)
    if p is None:
        raise HTTPException(status_code=404, detail="Provider not found")
    session.delete(p)
    session.commit()
    return {"deleted": provider_id}
