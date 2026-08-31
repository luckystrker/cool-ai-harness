"""FastAPI application entrypoint."""

from __future__ import annotations

from contextlib import asynccontextmanager
from pathlib import Path

from fastapi import Depends, FastAPI
from fastapi.middleware.cors import CORSMiddleware
from fastapi.responses import FileResponse
from fastapi.staticfiles import StaticFiles

from app.api.analytics import router as analytics_router
from app.api.artifacts import router as artifacts_router
from app.api.budgets import router as budgets_router
from app.api.constructor import router as constructor_router
from app.api.conversations import router as conversations_router
from app.api.inspector import router as inspector_router
from app.api.mcp import router as mcp_router
from app.api.memory import entities_router
from app.api.memory import router as memory_router
from app.api.plans import router as plans_router
from app.api.profiles import router as profiles_router
from app.api.providers import router as providers_router
from app.api.research import router as research_router
from app.api.routes import public_router as public_api_router
from app.api.routes import router as api_router
from app.api.rss import router as rss_router
from app.api.runs import router as runs_router
from app.api.skills import router as skills_router
from app.api.subagents import router as subagents_router
from app.api.tasks import router as tasks_router
from app.api.webhooks import public_router as webhooks_public_router
from app.api.webhooks import router as webhooks_router
from app.api.websocket import router as ws_router
from app.api.wiki import router as wiki_router
from app.api.workspace import router as workspace_router
from app.core.auth import require_auth
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
    )
    init_db()
    log.info("app.db_ready", database_url=settings.database_url)

    # Seed built-in subagent roles (Фаза 2 §5).
    from sqlmodel import Session

    from app.agent.subagents import ensure_builtin_roles
    from app.core.db import engine

    with Session(engine) as session:
        ensure_builtin_roles(session)
        from app.agent.constructor import load_macro_tools

        load_macro_tools(session)
    log.info("app.subagent_roles_ready")

    # Load MCP server configs and connect (Фаза 2 §4).
    from app.mcp import get_mcp_registry, load_mcp_configs, register_mcp_tools

    mcp_configs = load_mcp_configs()
    if mcp_configs:
        mcp_registry = get_mcp_registry()
        mcp_registry.load_configs(mcp_configs)
        await mcp_registry.connect_all()
        register_mcp_tools()
        log.info("app.mcp_ready", servers=len(mcp_configs))

    # Start the recurring-task scheduler (Фаза 3b §1). Jobs are rebuilt from
    # the scheduled_tasks table, so schedules survive a restart.
    from app.tasks.scheduler import shutdown_scheduler, start_scheduler

    jobs = await start_scheduler()
    log.info("app.scheduler_ready", jobs=jobs)

    yield

    await shutdown_scheduler()

    from app.tools.browser_tools import browser_sessions

    await browser_sessions.close_all()

    # Shutdown MCP connections.
    from app.mcp import get_mcp_registry as _get_mcp_reg

    await _get_mcp_reg().shutdown()
    log.info("app.shutdown")


def _mount_frontend(app: FastAPI, dist: Path) -> None:
    """Serve the Vite-built SPA from *dist* if the directory exists.

    Static assets (JS/CSS/images with content hashes) are mounted at /assets.
    All other non-API GET routes fall back to index.html so that client-side
    routing works on full-page refresh.
    """
    if not dist.is_dir():
        return

    index = dist / "index.html"
    if not index.is_file():
        return

    # Hashed build artifacts — long-cache safe.
    assets_dir = dist / "assets"
    if assets_dir.is_dir():
        app.mount("/assets", StaticFiles(directory=assets_dir), name="static-assets")

    # Serve other root-level files (favicon.svg, icons.svg, etc.)
    _dist_resolved = dist.resolve()

    @app.get("/{full_path:path}", include_in_schema=False)
    async def spa_fallback(full_path: str):
        """Return index.html for SPA routes, or the exact static file if it exists."""
        # Try to serve an exact file first (e.g. /favicon.svg).
        # Path-traversal guard: resolve and confine to the dist directory.
        candidate = (dist / full_path).resolve()
        if full_path and candidate.is_file() and candidate.is_relative_to(_dist_resolved):
            return FileResponse(candidate)
        return FileResponse(index)


def create_app() -> FastAPI:
    settings = get_settings()
    app = FastAPI(
        title=settings.app_name,
        version=settings.app_version,
        lifespan=lifespan,
    )

    # CORS: when credentials are enabled, browsers reject wildcard origins.
    # Use an explicit list; fall back to localhost dev ports.
    cors_origins = settings.cors_origins
    if "*" in cors_origins:
        cors_origins = ["http://localhost:5173", "http://127.0.0.1:5173", "http://localhost:4173"]
    app.add_middleware(
        CORSMiddleware,
        allow_origins=cors_origins,
        allow_credentials=True,
        allow_methods=["*"],
        allow_headers=["*"],
    )

    # Global auth dependency: all /api routes require a valid bearer token
    # when API_TOKEN is configured. Dev mode (empty token) passes through.
    _auth = [Depends(require_auth)]

    app.include_router(public_api_router, prefix="/api")
    app.include_router(api_router, prefix="/api", dependencies=_auth)
    app.include_router(conversations_router, prefix="/api", dependencies=_auth)
    app.include_router(constructor_router, prefix="/api", dependencies=_auth)
    app.include_router(runs_router, prefix="/api", dependencies=_auth)
    app.include_router(inspector_router, prefix="/api", dependencies=_auth)
    app.include_router(plans_router, prefix="/api", dependencies=_auth)
    app.include_router(providers_router, prefix="/api", dependencies=_auth)
    app.include_router(budgets_router, prefix="/api", dependencies=_auth)
    app.include_router(artifacts_router, prefix="/api", dependencies=_auth)
    app.include_router(skills_router, prefix="/api", dependencies=_auth)
    app.include_router(mcp_router, prefix="/api", dependencies=_auth)
    app.include_router(subagents_router, prefix="/api", dependencies=_auth)
    app.include_router(tasks_router, prefix="/api", dependencies=_auth)
    app.include_router(rss_router, prefix="/api", dependencies=_auth)
    app.include_router(webhooks_router, prefix="/api", dependencies=_auth)
    app.include_router(webhooks_public_router, prefix="/api")  # no auth: HMAC-secured
    app.include_router(profiles_router, prefix="/api", dependencies=_auth)
    app.include_router(memory_router, prefix="/api", dependencies=_auth)
    app.include_router(entities_router, prefix="/api", dependencies=_auth)
    app.include_router(workspace_router, prefix="/api", dependencies=_auth)
    app.include_router(analytics_router, prefix="/api", dependencies=_auth)
    app.include_router(wiki_router, prefix="/api", dependencies=_auth)
    app.include_router(research_router, prefix="/api", dependencies=_auth)
    app.include_router(ws_router)  # WebSocket routes live at /ws/... (token via query param)

    # --- Serve built frontend (SPA) if dist/ exists ---
    _mount_frontend(app, settings.frontend_dist)

    return app


app = create_app()
