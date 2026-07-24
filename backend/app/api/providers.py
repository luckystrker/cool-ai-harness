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
from app.providers import build_provider_from_form, build_provider_from_row

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


class ModelOut(BaseModel):
    """One model a provider serves, with whatever metadata is available."""

    id: str
    context_window: int | None = None
    prompt_price: float | None = None
    completion_price: float | None = None


class ModelsPreviewRequest(BaseModel):
    """Live model-list probe for an unsaved provider (create form).

    Lets the user pick a model from the provider's real ``/models`` response
    *before* saving. Carries the plaintext key once, in-memory only — it is
    never persisted by this path.
    """

    name: str = Field(..., description="openai | openrouter | deepseek | groq | ollama | anthropic | …")
    base_url: str | None = None
    api_key: str


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


async def _models_to_out(provider) -> list[ModelOut]:
    """Call a provider's list_models() and map to ModelOut, with sane errors."""
    try:
        models = await provider.list_models()
    except NotImplementedError as exc:
        raise HTTPException(
            status_code=501,
            detail=f"This provider does not expose a model list: {exc}",
        )
    except Exception as exc:  # noqa: BLE001 — surface provider errors to the UI
        log.warning("providers.list_models_failed", error=str(exc))
        raise HTTPException(
            status_code=502,
            detail=f"Could not list models from provider: {exc}",
        )
    return [
        ModelOut(
            id=m.id,
            context_window=m.context_window,
            prompt_price=m.prompt_price,
            completion_price=m.completion_price,
        )
        for m in models
    ]


@router.post("/providers/models/preview", response_model=list[ModelOut])
async def preview_provider_models(body: ModelsPreviewRequest) -> list[ModelOut]:
    """List models from a provider using raw form fields (no DB row yet).

    Used by the create-provider form so the user can select a model from the
    live list before saving. The submitted API key is used in-memory only.
    """
    provider = build_provider_from_form(
        name=body.name, base_url=body.base_url, api_key=body.api_key
    )
    return await _models_to_out(provider)


@router.get("/providers/{provider_id}/models", response_model=list[ModelOut])
async def list_provider_models(
    provider_id: int, session: Session = Depends(get_session)
) -> list[ModelOut]:
    """List models from an already-saved provider (edit form / chat picker)."""
    p = session.get(Provider, provider_id)
    if p is None:
        raise HTTPException(status_code=404, detail="Provider not found")
    provider = build_provider_from_row(p)
    return await _models_to_out(provider)


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
