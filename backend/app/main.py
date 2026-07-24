"""FastAPI application entrypoint."""

from __future__ import annotations

from contextlib import asynccontextmanager

from fastapi import FastAPI
from fastapi.middleware.cors import CORSMiddleware

from app.api.artifacts import router as artifacts_router
from app.api.budgets import router as budgets_router
from app.api.conversations import router as conversations_router
from app.api.providers import router as providers_router
from app.api.routes import router as api_router
from app.api.runs import router as runs_router
from app.api.websocket import router as ws_router
from app.api.workspace import router as workspace_router
from app.core.config import get_settings
from app.core.db import init_db
from app.core.logging import configure_logging, get_logger


@asynccontextmanager
async def lifespan(app: FastAPI):
    """Startup/shutdown lifecycle."""
    configure_logging()
    log = get_logger("app.main")
    settings = get_settings()
    log.info(
        "app.startup",
        app=settings.app_name,
        version=settings.app_version,
        env=settings.environment,
        default_provider=settings.default_provider,
        default_model=settings.default_model,
    )
    init_db()
    log.info("app.db_ready", database_url=settings.database_url)
    yield
    log.info("app.shutdown")


def create_app() -> FastAPI:
    settings = get_settings()
    app = FastAPI(
        title=settings.app_name,
        version=settings.app_version,
        lifespan=lifespan,
    )

    app.add_middleware(
        CORSMiddleware,
        allow_origins=settings.cors_origins,
        allow_credentials=True,
        allow_methods=["*"],
        allow_headers=["*"],
    )

    app.include_router(api_router, prefix="/api")
    app.include_router(conversations_router, prefix="/api")
    app.include_router(runs_router, prefix="/api")
    app.include_router(providers_router, prefix="/api")
    app.include_router(budgets_router, prefix="/api")
    app.include_router(artifacts_router, prefix="/api")
    app.include_router(workspace_router, prefix="/api")
    app.include_router(ws_router)  # WebSocket routes live at /ws/...

    return app


app = create_app()
