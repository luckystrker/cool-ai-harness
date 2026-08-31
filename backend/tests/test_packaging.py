"""M2 unified-package contract and same-origin smoke tests."""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest
import yaml
from fastapi.testclient import TestClient

from tests.conftest import ScriptedProvider

_PATH_ENV = (
    "COOL_CONFIG_FILE",
    "DATA_DIR",
    "WORKSPACES_DIR",
    "SKILLS_DIR",
    "ARTIFACTS_DIR",
    "FRONTEND_DIST",
)


def test_runtime_paths_derive_from_cool_home(tmp_path: Path, monkeypatch) -> None:
    from app.core.config import Settings

    for name in _PATH_ENV:
        monkeypatch.delenv(name, raising=False)

    home = tmp_path / "cool-home"
    settings = Settings(_env_file=None, cool_home=home, database_url="")

    assert settings.cool_home == home.resolve()
    assert settings.cool_config_file == (home / "config.yaml").resolve()
    assert settings.data_dir == (home / "data").resolve()
    assert settings.workspaces_dir == (home / "workspaces").resolve()
    assert settings.skills_dir == (home / "skills").resolve()
    assert settings.artifacts_dir == (home / "data" / "artifacts").resolve()
    assert settings.database_url == f"sqlite:///{(home / 'data' / 'harness.db').as_posix()}"


def test_env_example_loads_with_comma_separated_cors(monkeypatch) -> None:
    from app.core.config import Settings

    monkeypatch.delenv("DATABASE_URL", raising=False)
    repo = Path(__file__).resolve().parents[2]
    settings = Settings(_env_file=repo / ".env.example")

    assert settings.cors_origins == [
        "http://localhost:5173",
        "http://127.0.0.1:5173",
        "http://localhost:4173",
    ]
    assert settings.database_url.endswith("/data/harness.db")


def test_legacy_default_database_url_follows_cool_home(tmp_path: Path) -> None:
    from app.core.config import Settings

    home = tmp_path / "cool-home"
    settings = Settings(
        _env_file=None,
        cool_home=home,
        database_url="sqlite:///./data/harness.db",
    )

    assert settings.database_url == f"sqlite:///{(home / 'data' / 'harness.db').as_posix()}"


def test_relative_runtime_overrides_stay_below_cool_home(tmp_path: Path) -> None:
    from app.core.config import Settings

    home = tmp_path / "cool-home"
    settings = Settings(
        _env_file=None,
        cool_home=home,
        cool_config_file="configuration/cool.yaml",
        data_dir="state",
        workspaces_dir="projects",
        skills_dir="extensions/skills",
        artifacts_dir="state/blobs",
        frontend_dist="ui",
        database_url="sqlite:///explicit.db",
    )

    assert settings.cool_config_file == (home / "configuration" / "cool.yaml").resolve()
    assert settings.data_dir == (home / "state").resolve()
    assert settings.workspaces_dir == (home / "projects").resolve()
    assert settings.skills_dir == (home / "extensions" / "skills").resolve()
    assert settings.artifacts_dir == (home / "state" / "blobs").resolve()
    assert settings.frontend_dist == (home / "ui").resolve()
    assert settings.database_url == "sqlite:///explicit.db"


def test_cool_serve_is_single_worker_and_does_not_trust_proxy_by_default(monkeypatch) -> None:
    from app.cli import main

    captured: dict[str, Any] = {}

    def fake_run(app: str, **kwargs: Any) -> None:
        captured["app"] = app
        captured.update(kwargs)

    monkeypatch.setattr("uvicorn.run", fake_run)

    assert main(["serve", "--host", "127.0.0.1", "--port", "8765"]) == 0
    assert captured == {
        "app": "app.main:app",
        "host": "127.0.0.1",
        "port": 8765,
        "log_level": "info",
        "proxy_headers": False,
        "forwarded_allow_ips": "",
        "workers": 1,
    }


def _patch_provider(monkeypatch, provider: ScriptedProvider) -> None:
    monkeypatch.setattr("app.providers.get_provider_for_model", lambda model=None: provider)

    import app.api.conversations as conversations_module
    import app.api.websocket as websocket_module

    monkeypatch.setattr(
        conversations_module, "get_provider_for_model", lambda model=None: provider
    )
    monkeypatch.setattr(websocket_module, "get_provider_for_model", lambda model=None: provider)


def test_packaged_app_serves_spa_health_sse_and_websocket(
    tmp_path: Path, monkeypatch
) -> None:
    from app.core.config import get_settings
    from app.main import create_app

    dist = tmp_path / "dist"
    dist.mkdir()
    marker = "m2-package-smoke"
    (dist / "index.html").write_text(f"<html>{marker}</html>", encoding="utf-8")
    monkeypatch.setattr(get_settings(), "frontend_dist", dist)

    provider = ScriptedProvider()
    provider.set_script(["SSE package smoke", "WebSocket package smoke"])
    _patch_provider(monkeypatch, provider)

    with TestClient(create_app()) as client:
        root = client.get("/")
        assert root.status_code == 200
        assert marker in root.text

        health = client.get("/api/health")
        assert health.status_code == 200
        assert health.json()["status"] == "ok"

        conversation = client.post(
            "/api/conversations", json={"title": "M2 package smoke"}
        ).json()
        conversation_id = conversation["id"]

        with client.stream(
            "POST",
            f"/api/conversations/{conversation_id}/messages",
            json={"content": "SSE"},
            headers={"Accept": "text/event-stream"},
        ) as response:
            assert response.status_code == 200
            sse_events = [
                json.loads(line.removeprefix("data:").strip())
                for line in response.iter_lines()
                if line.startswith("data:")
            ]

        assert any(event["kind"] == "token" for event in sse_events)
        assert sse_events[-1]["kind"] == "finish"

        with client.websocket_connect(f"/ws/chat/{conversation_id}") as socket:
            socket.send_text(json.dumps({"content": "WebSocket"}))
            websocket_events = []
            while True:
                event = json.loads(socket.receive_text())
                websocket_events.append(event)
                if event["kind"] == "finish":
                    break

    assert any(event["kind"] == "token" for event in websocket_events)
    assert websocket_events[-1]["kind"] == "finish"


def test_package_health_stays_public_when_api_compat_token_is_set(
    tmp_path: Path, monkeypatch
) -> None:
    from app.core.config import get_settings
    from app.main import create_app

    dist = tmp_path / "dist"
    dist.mkdir()
    (dist / "index.html").write_text("<html>secured smoke</html>", encoding="utf-8")
    settings = get_settings()
    monkeypatch.setattr(settings, "frontend_dist", dist)
    monkeypatch.setattr(settings, "api_token", "api-client-only")

    with TestClient(create_app()) as client:
        assert client.get("/").status_code == 200
        assert client.get("/api/health").status_code == 200
        assert client.get("/api/conversations").status_code == 401


def test_empty_production_database_upgrades_to_alembic_head(
    tmp_path: Path, monkeypatch
) -> None:
    from sqlalchemy import create_engine, text

    from app.core import db as db_module
    from app.core.config import get_settings

    database = tmp_path / "production.db"
    database_url = f"sqlite:///{database.as_posix()}"
    production_engine = create_engine(database_url)
    monkeypatch.setattr(db_module, "engine", production_engine)
    monkeypatch.setattr(get_settings(), "database_url", database_url)

    db_module._run_alembic_upgrade()

    with production_engine.connect() as connection:
        revision = connection.execute(text("SELECT version_num FROM alembic_version")).scalar_one()
    assert revision == "0022"


def test_current_unversioned_dev_database_fails_closed_in_production(
    tmp_path: Path, monkeypatch
) -> None:
    from sqlalchemy import create_engine, inspect
    from sqlmodel import SQLModel

    from app import models  # noqa: F401
    from app.core import db as db_module
    from app.core.config import get_settings
    from app.memory import models as memory_models  # noqa: F401

    database = tmp_path / "legacy-dev.db"
    database_url = f"sqlite:///{database.as_posix()}"
    legacy_engine = create_engine(database_url)
    SQLModel.metadata.create_all(legacy_engine)
    assert not inspect(legacy_engine).has_table("alembic_version")

    monkeypatch.setattr(db_module, "engine", legacy_engine)
    monkeypatch.setattr(get_settings(), "database_url", database_url)
    tables_before = set(inspect(legacy_engine).get_table_names())
    with pytest.raises(RuntimeError, match="refusing automatic Alembic baseline"):
        db_module._run_alembic_upgrade()

    inspector = inspect(legacy_engine)
    assert not inspector.has_table("alembic_version")
    assert set(inspector.get_table_names()) == tables_before


def test_partial_unversioned_database_is_not_stamped(tmp_path: Path, monkeypatch) -> None:
    from sqlalchemy import create_engine, inspect, text

    from app.core import db as db_module
    from app.core.config import get_settings

    database = tmp_path / "partial.db"
    database_url = f"sqlite:///{database.as_posix()}"
    partial_engine = create_engine(database_url)
    with partial_engine.begin() as connection:
        connection.execute(text("CREATE TABLE users (id INTEGER PRIMARY KEY)"))

    monkeypatch.setattr(db_module, "engine", partial_engine)
    monkeypatch.setattr(get_settings(), "database_url", database_url)

    with pytest.raises(RuntimeError, match="refusing automatic Alembic baseline"):
        db_module._run_alembic_upgrade()

    assert not inspect(partial_engine).has_table("alembic_version")


def test_release_image_and_compose_keep_one_service_and_one_entrypoint() -> None:
    repo = Path(__file__).resolve().parents[2]
    dockerfile = (repo / "Dockerfile").read_text(encoding="utf-8")
    compose = yaml.safe_load((repo / "docker-compose.yml").read_text(encoding="utf-8"))

    assert "FROM node:22-bookworm-slim AS frontend-build" in dockerfile
    assert "COPY --from=frontend-build" in dockerfile
    assert "COPY skills /opt/cool/skills" in dockerfile
    assert "git" in dockerfile
    assert "USER cool" in dockerfile
    assert 'CMD ["cool", "serve", "--host", "0.0.0.0", "--port", "8000"]' in dockerfile
    assert not (repo / "backend" / "Dockerfile").exists()

    assert list(compose["services"]) == ["cool"]
    assert compose["services"]["cool"]["env_file"] == [
        {"path": ".env", "required": False}
    ]
    assert compose["services"]["cool"]["ports"] == [
        "127.0.0.1:${COOL_PORT:-8000}:8000"
    ]
    assert compose["services"]["cool"]["command"] == [
        "cool",
        "serve",
        "--host",
        "0.0.0.0",
        "--port",
        "8000",
    ]
    assert compose["services"]["cool"]["volumes"] == ["cool-state:/var/lib/cool"]
    assert "DATABASE_URL" not in compose["services"]["cool"]["environment"]
