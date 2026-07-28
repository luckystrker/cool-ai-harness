"""Shared pytest fixtures."""

from __future__ import annotations

import os
import tempfile
from collections.abc import AsyncIterator
from pathlib import Path
from typing import Any

import pytest

# Point the app at a throwaway database BEFORE any `app.*` import. The engine
# is created at module-import time (app.core.db) from DATABASE_URL, and
# running the suite against data/harness.db pollutes production data with
# test conversations/providers (this is how the /tmp/agent-x working dirs and
# gpt-4o provider rows leaked into the real DB and surfaced in the chat UI).
# Env vars take precedence over the .env file in pydantic-settings.
# When running under pytest-xdist, each worker gets its own DB file to avoid
# SQLite locking contention between parallel processes.
_worker_id = os.environ.get("PYTEST_XDIST_WORKER", "")
_db_suffix = f"_{_worker_id}" if _worker_id else ""
_TEST_DB = Path(tempfile.gettempdir()) / f"cool_ai_harness_test{_db_suffix}.db"
if _TEST_DB.exists():
    _TEST_DB.unlink()
os.environ["DATABASE_URL"] = f"sqlite:///{_TEST_DB}"

from app.providers import ChatStreamEvent, LLMProvider, Message, ToolSpec, Usage  # noqa: E402


class ScriptedProvider(LLMProvider):
    """A test double provider that replays a queued list of "turns".

    Each turn is either:
      - a string → streamed as text tokens, then a finish event
      - a list of tool calls → streamed as tool_call deltas, finish, then
        the loop executes the tools and re-enters the loop
      - a dict {"text": str, "reasoning": str, "tool_calls": [...]} → any combo;
        ``reasoning`` is streamed as ChatStreamEvent.reasoning chunks

    The provider records every call so tests can assert on history.
    """

    name = "scripted"

    def __init__(self, *, default_model: str = "test-model") -> None:
        self.default_model = default_model
        self.turns: list[Any] = []
        self.calls: list[list[Message]] = []

    def set_script(self, turns: list[Any]) -> None:
        self.turns = list(turns)
    async def chat_completion(self, messages, *, model, tools=None, **kwargs):  # type: ignore[override]
        raise NotImplementedError("ScriptedProvider only implements streaming")

    async def chat_completion_stream(  # type: ignore[override]
        self,
        messages: list[Message],
        *,
        model: str,
        tools: list[ToolSpec] | None = None,
        temperature: float = 0.7,
        max_tokens: int | None = None,
        **kwargs: Any,
    ) -> AsyncIterator[ChatStreamEvent]:
        self.calls.append(list(messages))
        if not self.turns:
            raise RuntimeError("ScriptedProvider: script exhausted")
        turn = self.turns.pop(0)

        text: str = ""
        reasoning: str = ""
        tool_calls: list[dict[str, Any]] | None = None
        if isinstance(turn, str):
            text = turn
        elif isinstance(turn, list):
            tool_calls = turn
        elif isinstance(turn, dict):
            text = turn.get("text", "")
            reasoning = turn.get("reasoning", "")
            tool_calls = turn.get("tool_calls")
        else:
            raise TypeError(f"Bad scripted turn: {turn!r}")

        if reasoning:
            for word in reasoning.split(" "):
                yield ChatStreamEvent(reasoning=word + " ")
        if text:
            for word in text.split(" "):
                yield ChatStreamEvent(delta=word + " ")
        if tool_calls:
            for idx, call in enumerate(tool_calls):
                args = call.get("arguments", {})
                import json as _json

                yield ChatStreamEvent(
                    tool_call_delta={
                        "index": idx,
                        "id": call.get("id", f"call_{idx}"),
                        "type": "function",
                        "function": {
                            "name": call["name"],
                            "arguments": _json.dumps(args),
                        },
                    }
                )
        yield ChatStreamEvent(finish=True, usage=Usage(prompt_tokens=10, completion_tokens=5, total_tokens=15))


@pytest.fixture(scope="session", autouse=True)
def _ensure_db_schema() -> None:
    """Create all tables on the throwaway test engine once per worker.

    Tests that reach the real ``app.core.db.engine`` (e.g. the executor budget
    block integration test) need the schema to exist. Without this, the test
    passes only when another test file happens to have called ``init_db()``
    first — a flaky ordering dependency that breaks under ``pytest-xdist
    -n auto --dist=loadfile`` where files are dispatched to workers
    independently.
    """
    from app.core.db import init_db

    init_db()


@pytest.fixture
def workspace(tmp_path: Path, monkeypatch) -> Path:
    """Redirect the workspace to a temp dir for isolated file-tool tests."""
    from app.core import config as config_module

    ws = tmp_path / "ws"
    ws.mkdir()
    settings = config_module.get_settings()
    monkeypatch.setattr(settings, "workspaces_dir", ws)
    return ws


@pytest.fixture
def scripted_provider() -> ScriptedProvider:
    return ScriptedProvider()
