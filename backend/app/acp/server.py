"""ACP v1 JSON-RPC connection backed by the existing durable runtime."""

from __future__ import annotations

import asyncio
from collections.abc import Awaitable, Callable, Mapping, Sequence
from contextlib import suppress
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from sqlalchemy.engine import Engine
from sqlmodel import Session

from app.acp.adapter import ACPEventAdapter
from app.agent.approvals import DEFAULT_APPROVAL_TIMEOUT_S, approval_registry
from app.agent.runners import run_conversation_turn
from app.agent.runs import conversation_turn_registry, run_registry
from app.agent.service import (
    append_message,
    create_conversation,
    create_run,
    get_conversation,
    get_or_create_default_user,
    list_messages,
    resolve_default_model,
)
from app.core.db import engine
from app.providers import LLMProvider, get_provider_for_model
from app.providers.registry import resolve_provider_model

ACP_PROTOCOL_VERSION = 1
ACP_AGENT_INFO = {"name": "cool-ai-harness", "title": "Cool AI Harness", "version": "0.1.0"}
JSONValue = dict[str, Any] | list[Any] | str | int | float | bool | None
Sender = Callable[[JSONValue], Awaitable[None]]
ProviderResolver = Callable[[str | None], LLMProvider]


class ACPError(Exception):
    """A JSON-RPC error safe to expose to an ACP client."""

    def __init__(self, code: int, message: str, data: Any | None = None) -> None:
        super().__init__(message)
        self.code = code
        self.message = message
        self.data = data


@dataclass
class _ActivePrompt:
    conversation_id: int
    run_id: int
    cancelled: bool = False


def _params(value: Any) -> dict[str, Any]:
    if value is None:
        return {}
    if not isinstance(value, Mapping):
        raise ACPError(-32602, "params must be an object")
    return dict(value)


def _required_string(params: Mapping[str, Any], key: str) -> str:
    value = params.get(key)
    if not isinstance(value, str) or not value:
        raise ACPError(-32602, f"{key} must be a non-empty string")
    return value


def _session_number(session_id: str) -> int:
    prefix = "conversation:"
    if not session_id.startswith(prefix):
        raise ACPError(-32002, "session not found")
    raw = session_id[len(prefix) :]
    if not raw.isascii() or not raw.isdecimal() or int(raw) < 1:
        raise ACPError(-32002, "session not found")
    return int(raw)


def _absolute_directory(value: Any, *, key: str = "cwd") -> str:
    if not isinstance(value, str) or not value:
        raise ACPError(-32602, f"{key} must be a non-empty absolute path")
    path = Path(value)
    if not path.is_absolute():
        raise ACPError(-32602, f"{key} must be an absolute path")
    if not path.is_dir():
        raise ACPError(-32002, f"{key} directory does not exist")
    return str(path.resolve())


def _reject_unsupported_roots_and_mcp(params: Mapping[str, Any]) -> None:
    additional = params.get("additionalDirectories", [])
    mcp_servers = params.get("mcpServers")
    if not isinstance(additional, list):
        raise ACPError(-32602, "additionalDirectories must be an array")
    if additional:
        raise ACPError(-32602, "additionalDirectories are not supported by this adapter")
    if not isinstance(mcp_servers, list):
        raise ACPError(-32602, "mcpServers must be an array")
    if mcp_servers:
        raise ACPError(-32602, "client-supplied MCP servers are not supported by this adapter")


def _prompt_text(value: Any) -> str:
    if not isinstance(value, list) or not value:
        raise ACPError(-32602, "prompt must be a non-empty array")
    parts: list[str] = []
    for block in value:
        if not isinstance(block, Mapping):
            raise ACPError(-32602, "prompt blocks must be objects")
        block_type = block.get("type")
        if block_type == "text":
            text = block.get("text")
            if not isinstance(text, str):
                raise ACPError(-32602, "text prompt blocks require a string text field")
            parts.append(text)
            continue
        if block_type == "resource_link":
            uri = block.get("uri")
            name = block.get("name")
            if not isinstance(uri, str) or not uri or not isinstance(name, str) or not name:
                raise ACPError(
                    -32602, "resource_link prompt blocks require non-empty uri and name fields"
                )
            # Resource links are references supplied by the client.  Preserve
            # them in model context without performing an implicit file read or
            # network fetch that could bypass normal capability checks.
            parts.append(f"[ACP resource: {name}]({uri})")
            continue
        raise ACPError(-32602, f"unsupported ACP prompt block type: {block_type!r}")
    combined = "\n".join(parts)
    if not combined.strip():
        raise ACPError(-32602, "prompt text must not be empty")
    return combined


class ACPConnection:
    """One bidirectional ACP connection.

    Request handling is concurrent so a client can answer a permission request
    or send ``session/cancel`` while ``session/prompt`` remains in flight.
    """

    def __init__(
        self,
        send: Sender,
        *,
        db_engine: Engine = engine,
        provider_resolver: ProviderResolver = get_provider_for_model,
        approval_timeout_s: float = DEFAULT_APPROVAL_TIMEOUT_S,
    ) -> None:
        self._send = send
        self._engine = db_engine
        self._provider_resolver = provider_resolver
        self._approval_timeout_s = approval_timeout_s
        self._initialized = False
        self._client_capabilities: dict[str, Any] = {}
        self._active: dict[str, _ActivePrompt] = {}
        self._tasks: set[asyncio.Task[None]] = set()
        self._pending_client_requests: dict[str, asyncio.Future[dict[str, Any]]] = {}
        self._permission_requests_by_session: dict[str, set[str]] = {}
        self._next_request_id = 0
        self._closed = False

    async def receive(self, message: JSONValue) -> None:
        """Accept one decoded JSON-RPC message or batch from the client."""
        if self._closed:
            return
        if isinstance(message, list):
            if not message:
                await self._send(self._error_response(None, ACPError(-32600, "invalid request")))
                return
            task = asyncio.create_task(self._serve_batch(message))
            self._track(task)
            return
        if not isinstance(message, Mapping):
            await self._send(self._error_response(None, ACPError(-32600, "invalid request")))
            return
        obj = dict(message)
        if "method" not in obj and "id" in obj:
            self._receive_client_response(obj)
            return
        if "id" in obj:
            task = asyncio.create_task(self._serve_request(obj))
            self._track(task)
            return
        await self._serve_notification(obj)

    async def drain(self) -> None:
        """Wait until all currently scheduled client requests finish."""
        while self._tasks:
            await asyncio.gather(*tuple(self._tasks), return_exceptions=True)

    async def close(self) -> None:
        """Cancel runtime work owned by the disconnected ACP connection."""
        if self._closed:
            return
        self._closed = True
        for session_id in list(self._active):
            self._cancel_session(session_id)
        for future in self._pending_client_requests.values():
            if not future.done():
                future.set_result({"outcome": {"outcome": "cancelled"}})
        if self._tasks:
            with suppress(asyncio.TimeoutError):
                await asyncio.wait_for(
                    asyncio.gather(*tuple(self._tasks), return_exceptions=True), timeout=5
                )

    def _track(self, task: asyncio.Task[None]) -> None:
        self._tasks.add(task)
        task.add_done_callback(self._tasks.discard)

    async def _serve_batch(self, batch: list[Any]) -> None:
        calls: list[Awaitable[dict[str, Any]]] = []
        immediate: list[dict[str, Any]] = []
        for item in batch:
            if not isinstance(item, Mapping):
                immediate.append(self._error_response(None, ACPError(-32600, "invalid request")))
                continue
            obj = dict(item)
            if "method" not in obj and "id" in obj:
                self._receive_client_response(obj)
            elif "id" in obj:
                calls.append(self._call_request(obj))
            else:
                await self._serve_notification(obj)
        responses = immediate + (list(await asyncio.gather(*calls)) if calls else [])
        if not responses:
            return
        await self._send(responses)

    async def _serve_request(self, obj: dict[str, Any]) -> None:
        await self._send(await self._call_request(obj))

    async def _call_request(self, obj: Mapping[str, Any]) -> dict[str, Any]:
        request_id = obj.get("id")
        if not self._valid_id(request_id) or obj.get("jsonrpc") != "2.0":
            return self._error_response(None, ACPError(-32600, "invalid request"))
        method = obj.get("method")
        if not isinstance(method, str):
            return self._error_response(request_id, ACPError(-32600, "invalid request"))
        try:
            result = await self._dispatch(method, _params(obj.get("params")))
            return {"jsonrpc": "2.0", "id": request_id, "result": result}
        except ACPError as exc:
            return self._error_response(request_id, exc)
        except Exception as exc:  # pragma: no cover - defensive protocol boundary
            return self._error_response(
                request_id, ACPError(-32603, "internal error", {"type": type(exc).__name__})
            )

    async def _serve_notification(self, obj: dict[str, Any]) -> None:
        if obj.get("jsonrpc") != "2.0" or not isinstance(obj.get("method"), str):
            return
        method = str(obj["method"])
        if method == "session/cancel":
            try:
                session_id = _required_string(_params(obj.get("params")), "sessionId")
            except ACPError:
                return
            self._cancel_session(session_id)

    async def _dispatch(self, method: str, params: dict[str, Any]) -> dict[str, Any]:
        if method == "initialize":
            return self._initialize(params)
        if not self._initialized:
            raise ACPError(-32600, "initialize must be called before session methods")
        if method == "session/new":
            return self._new_session(params)
        if method == "session/load":
            return await self._load_session(params)
        if method == "session/prompt":
            return await self._prompt(params)
        raise ACPError(-32601, "method not found")

    def _initialize(self, params: Mapping[str, Any]) -> dict[str, Any]:
        if self._initialized:
            raise ACPError(-32600, "initialize may only be called once")
        version = params.get("protocolVersion")
        if not isinstance(version, int) or isinstance(version, bool):
            raise ACPError(-32602, "protocolVersion must be an integer")
        capabilities = params.get("clientCapabilities", {})
        if not isinstance(capabilities, Mapping):
            raise ACPError(-32602, "clientCapabilities must be an object")
        self._initialized = True
        self._client_capabilities = dict(capabilities)
        return {
            "protocolVersion": ACP_PROTOCOL_VERSION,
            "agentCapabilities": {
                "loadSession": True,
                "promptCapabilities": {
                    "image": False,
                    "audio": False,
                    "embeddedContext": False,
                },
                "mcpCapabilities": {"http": False, "sse": False},
            },
            "authMethods": [],
            "agentInfo": ACP_AGENT_INFO,
        }

    def _new_session(self, params: Mapping[str, Any]) -> dict[str, Any]:
        cwd = _absolute_directory(params.get("cwd"))
        _reject_unsupported_roots_and_mcp(params)
        with Session(self._engine) as session:
            user = get_or_create_default_user(session)
            assert user.id is not None
            model = resolve_default_model(session)
            conversation = create_conversation(
                session,
                user_id=user.id,
                title="ACP session",
                model=model,
                working_directory=cwd,
            )
            assert conversation.id is not None
            return {"sessionId": f"conversation:{conversation.id}"}

    async def _load_session(self, params: Mapping[str, Any]) -> dict[str, Any]:
        session_id = _required_string(params, "sessionId")
        cwd = _absolute_directory(params.get("cwd"))
        _reject_unsupported_roots_and_mcp(params)
        conversation_id = _session_number(session_id)
        with Session(self._engine) as session:
            conversation = self._owned_conversation(session, conversation_id)
            stored_cwd = conversation.working_directory
            if stored_cwd is None or Path(stored_cwd).resolve() != Path(cwd).resolve():
                raise ACPError(-32602, "cwd must match the session working directory")
            messages = list(list_messages(session, conversation_id))
            title = conversation.title
        for message in messages:
            if message.role == "user" and message.content:
                await self._notify(
                    session_id, ACPEventAdapter.message_chunk(message.content, role="user")
                )
            elif message.role == "assistant":
                if message.thinking:
                    await self._notify(
                        session_id,
                        {
                            "sessionUpdate": "agent_thought_chunk",
                            "content": {"type": "text", "text": message.thinking},
                        },
                    )
                if message.content:
                    await self._notify(
                        session_id, ACPEventAdapter.message_chunk(message.content, role="agent")
                    )
                for raw_call in message.tool_calls or []:
                    call = dict(raw_call)
                    function = call.get("function")
                    if isinstance(function, Mapping):
                        name = str(function.get("name") or "Tool")
                        arguments = function.get("arguments")
                    else:
                        name = str(call.get("name") or "Tool")
                        arguments = call.get("arguments")
                    await self._notify(
                        session_id,
                        {
                            "sessionUpdate": "tool_call",
                            "toolCallId": str(call.get("id") or f"message:{message.id}"),
                            "title": name,
                            "kind": "other",
                            "status": "pending",
                            "rawInput": arguments,
                        },
                    )
            elif message.role == "tool" and message.tool_result:
                call_id = str(message.tool_result.get("tool_call_id") or f"message:{message.id}")
                await self._notify(
                    session_id,
                    {
                        "sessionUpdate": "tool_call_update",
                        "toolCallId": call_id,
                        "status": "completed",
                        "rawOutput": message.tool_result,
                    },
                )
        result: dict[str, Any] = {}
        if title:
            result["_meta"] = {"io.github.luckystrker.cool/title": title}
        return result

    async def _prompt(self, params: Mapping[str, Any]) -> dict[str, Any]:
        session_id = _required_string(params, "sessionId")
        if session_id in self._active:
            raise ACPError(-32600, "a prompt is already active for this session")
        conversation_id = _session_number(session_id)
        text = _prompt_text(params.get("prompt"))
        lease = conversation_turn_registry.acquire(conversation_id)
        if lease is None:
            raise ACPError(-32000, "another prompt is already active for this session")

        try:
            with Session(self._engine) as session:
                conversation = self._owned_conversation(session, conversation_id)
                requested_model = conversation.model or resolve_default_model(session)
                provider = self._provider_resolver(requested_model)
                model = resolve_provider_model(provider, requested_model)
                if model is None:
                    raise ACPError(-32602, "no default model is configured")
                try:
                    append_message(
                        session,
                        conversation_id=conversation_id,
                        role="user",
                        content=text,
                        commit=False,
                    )
                    run = create_run(
                        session,
                        conversation_id=conversation_id,
                        user_id=conversation.user_id,
                        model=model,
                        commit=False,
                    )
                    session.commit()
                    session.refresh(run)
                except Exception:
                    session.rollback()
                    raise
                assert run.id is not None
                active = _ActivePrompt(conversation_id=conversation_id, run_id=run.id)
                self._active[session_id] = active
                adapter = ACPEventAdapter()
                stop_reason = "end_turn"
                try:
                    async for event in run_conversation_turn(
                        session=session,
                        conversation_id=conversation_id,
                        provider=provider,
                        model=model,
                        user_input=None,
                        working_directory=conversation.working_directory,
                        conversation_permissions=conversation.permissions,
                        conversation_capability_policy=conversation.capability_policy,
                        conversation_breakpoints=(conversation.metadata_ or {}).get("breakpoints"),
                        run_id=run.id,
                        cancellable=True,
                        profile_id=conversation.profile_id,
                    ):
                        envelope = event.to_canonical_dict()
                        for update in adapter.adapt(envelope):
                            await self._notify(session_id, update)
                        canonical = dict(envelope["event"])
                        if canonical.get("kind") == "tool.approval_required":
                            await self._resolve_permission(
                                session_id,
                                conversation.user_id,
                                conversation_id,
                                run.id,
                                adapter,
                                dict(canonical.get("payload") or {}),
                            )
                        if event.kind == "finish":
                            stop_reason = self._stop_reason(event.payload.get("reason"))
                        elif event.kind == "error":
                            stop_reason = "refusal"
                finally:
                    self._active.pop(session_id, None)
                    self._cancel_pending_permission_requests(session_id)
                if active.cancelled:
                    stop_reason = "cancelled"
                return {"stopReason": stop_reason}
        finally:
            conversation_turn_registry.release(conversation_id, lease)

    async def _resolve_permission(
        self,
        session_id: str,
        user_id: int,
        conversation_id: int,
        run_id: int,
        adapter: ACPEventAdapter,
        payload: Mapping[str, Any],
    ) -> None:
        approval_id = _required_string(payload, "approvalId")
        revision = payload.get("revision")
        if not isinstance(revision, int) or isinstance(revision, bool) or revision < 1:
            raise ACPError(-32603, "canonical approval revision is invalid")
        result = await self._request_client(
            session_id,
            "session/request_permission",
            {
                "sessionId": session_id,
                "toolCall": adapter.permission_tool_call(payload),
                "options": [
                    {"optionId": "allow_once", "name": "Allow once", "kind": "allow_once"},
                    {"optionId": "reject_once", "name": "Reject", "kind": "reject_once"},
                ],
            },
        )
        outcome = result.get("outcome")
        approved = (
            isinstance(outcome, Mapping)
            and outcome.get("outcome") == "selected"
            and outcome.get("optionId") == "allow_once"
        )
        approval_registry.resolve(
            approval_id,
            approved,
            expected_revision=revision,
            actor_id=user_id,
            conversation_id=conversation_id,
            run_id=run_id,
        )

    async def _request_client(
        self, session_id: str, method: str, params: dict[str, Any]
    ) -> dict[str, Any]:
        self._next_request_id += 1
        request_id = f"cool-{self._next_request_id}"
        future: asyncio.Future[dict[str, Any]] = asyncio.get_running_loop().create_future()
        self._pending_client_requests[request_id] = future
        self._permission_requests_by_session.setdefault(session_id, set()).add(request_id)
        await self._send({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params})
        try:
            return await asyncio.wait_for(future, timeout=self._approval_timeout_s)
        except TimeoutError:
            return {"outcome": {"outcome": "cancelled"}}
        finally:
            self._pending_client_requests.pop(request_id, None)
            self._permission_requests_by_session.get(session_id, set()).discard(request_id)

    def _receive_client_response(self, obj: Mapping[str, Any]) -> None:
        request_id = obj.get("id")
        if not isinstance(request_id, str):
            return
        future = self._pending_client_requests.get(request_id)
        if future is None or future.done():
            return
        if obj.get("jsonrpc") != "2.0" or ("result" in obj) == ("error" in obj):
            future.set_result({"outcome": {"outcome": "cancelled"}})
            return
        result = obj.get("result")
        if not isinstance(result, Mapping):
            future.set_result({"outcome": {"outcome": "cancelled"}})
            return
        outcome = result.get("outcome")
        if not isinstance(outcome, Mapping):
            future.set_result({"outcome": {"outcome": "cancelled"}})
            return
        outcome_name = outcome.get("outcome")
        if outcome_name == "selected" and isinstance(outcome.get("optionId"), str):
            future.set_result(dict(result))
            return
        if outcome_name == "cancelled":
            future.set_result({"outcome": {"outcome": "cancelled"}})
            return
        future.set_result({"outcome": {"outcome": "cancelled"}})

    def _cancel_session(self, session_id: str) -> None:
        active = self._active.get(session_id)
        if active is None:
            return
        active.cancelled = True
        approval_registry.cancel_for_run(active.run_id, conversation_id=active.conversation_id)
        run_registry.cancel(active.run_id)
        self._cancel_pending_permission_requests(session_id)

    def _cancel_pending_permission_requests(self, session_id: str) -> None:
        for request_id in tuple(self._permission_requests_by_session.pop(session_id, set())):
            future = self._pending_client_requests.get(request_id)
            if future is not None and not future.done():
                future.set_result({"outcome": {"outcome": "cancelled"}})

    async def _notify(self, session_id: str, update: dict[str, Any]) -> None:
        await self._send(
            {
                "jsonrpc": "2.0",
                "method": "session/update",
                "params": {"sessionId": session_id, "update": update},
            }
        )

    def _owned_conversation(self, session: Session, conversation_id: int):
        conversation = get_conversation(session, conversation_id)
        if conversation is None:
            raise ACPError(-32002, "session not found")
        user = get_or_create_default_user(session)
        if conversation.user_id != user.id:
            raise ACPError(-32002, "session not found")
        return conversation

    @staticmethod
    def _valid_id(value: Any) -> bool:
        return isinstance(value, str) or (isinstance(value, int) and not isinstance(value, bool))

    @staticmethod
    def _stop_reason(reason: Any) -> str:
        value = str(reason or "")
        if value in {"cancelled", "canceled"}:
            return "cancelled"
        if value in {"max_tokens", "length"}:
            return "max_tokens"
        if value in {"max_iterations", "max_turns"}:
            return "max_turn_requests"
        if value in {"error", "failed", "denied"}:
            return "refusal"
        return "end_turn"

    @staticmethod
    def _error_response(request_id: Any, exc: ACPError) -> dict[str, Any]:
        error: dict[str, Any] = {"code": exc.code, "message": exc.message}
        if exc.data is not None:
            error["data"] = exc.data
        return {"jsonrpc": "2.0", "id": request_id, "error": error}


__all__: Sequence[str] = (
    "ACP_AGENT_INFO",
    "ACP_PROTOCOL_VERSION",
    "ACPConnection",
    "ACPError",
)
