"""Pydantic request/response schemas for the HTTP API."""

from __future__ import annotations

from datetime import datetime
from typing import Any

from pydantic import BaseModel, Field

# --- health ---


class HealthResponse(BaseModel):
    status: str = "ok"
    version: str
    environment: str


# --- chat (MVP smoke endpoint) ---


class ChatMessageIn(BaseModel):
    role: str = Field(default="user")
    content: str


class ChatRequest(BaseModel):
    messages: list[ChatMessageIn]
    model: str | None = None
    temperature: float = 0.7
    max_tokens: int | None = None


class UsageOut(BaseModel):
    prompt_tokens: int = 0
    completion_tokens: int = 0
    total_tokens: int = 0


class ChatResponse(BaseModel):
    content: str | None
    model: str
    usage: UsageOut | None = None
    finish_reason: str | None = None
    raw: Any | None = None


# --- conversations ---


class ConversationCreate(BaseModel):
    title: str | None = None
    system_prompt: str | None = None
    model: str | None = None
    tool_names: list[str] | None = None
    # Per-conversation working directory (overrides the global default).
    working_directory: str | None = None
    # Per-conversation tool permissions: {"*": "ask", "read_file": "allow", ...}
    # Each value must be one of allow|ask|deny. Validated by the route layer.
    permissions: dict[str, str] | None = None
    # Per-conversation capability policy: {"execute": "ask", "network": "ask", ...}
    # Each value must be one of allow|ask|deny. Validated by the route layer.
    capability_policy: dict[str, str] | None = None
    # Per-conversation breakpoints: [{"type": "before_write", "tool": "write_file"}, ...]
    # Stored in conversation metadata. See app/security/breakpoints.py.
    breakpoints: list[dict[str, Any]] | None = None


class ConversationUpdate(BaseModel):
    # All fields optional; only provided fields are applied.
    title: str | None = None
    model: str | None = None
    working_directory: str | None = None
    permissions: dict[str, str] | None = None
    capability_policy: dict[str, str] | None = None
    breakpoints: list[dict[str, Any]] | None = None


class ConversationOut(BaseModel):
    id: int
    user_id: int
    title: str | None = None
    model: str | None = None
    working_directory: str | None = None
    permissions: dict[str, str] | None = None
    capability_policy: dict[str, str] | None = None
    breakpoints: list[dict[str, Any]] | None = None
    created_at: datetime
    updated_at: datetime


class MessageOut(BaseModel):
    id: int
    conversation_id: int
    role: str
    content: str | None = None
    tool_calls: list[dict[str, Any]] | None = None
    usage: dict[str, Any] | None = None
    thinking: str | None = None
    tool_result: dict[str, Any] | None = None
    # Which model produced this assistant message + how long the turn took.
    model: str | None = None
    duration_ms: int | None = None
    created_at: datetime


class ConversationDetail(ConversationOut):
    messages: list[MessageOut] = []


class SendMessageRequest(BaseModel):
    content: str
    # Optional per-message overrides.
    model: str | None = None
    system_prompt: str | None = None
    tool_names: list[str] | None = None
    # When True, the agent generates a structured plan instead of executing
    # directly (Фаза 2 §1 Planning Mode).
    plan_mode: bool = False


class ToolApprovalRequest(BaseModel):
    """Client decision for a pending tool-call approval.

    ``call_id`` may also be supplied in the URL path; the body value wins if
    both are present. ``approved=False`` denies the call (the loop continues
    with a Permission-denied tool_result).
    """

    approved: bool


# --- agent runs (Фаза 1.5 — durable runs) ---


class RunEventOut(BaseModel):
    """One row of a run's append-only event log."""

    id: int
    run_id: int
    seq: int
    kind: str
    payload: dict[str, Any] | None = None
    created_at: datetime


class RunOut(BaseModel):
    """Summary of a run (list/detail)."""

    id: int
    conversation_id: int
    status: str
    model: str | None = None
    iterations: int = 0
    usage: dict[str, Any] | None = None
    finish_reason: str | None = None
    error: str | None = None
    started_at: datetime
    finished_at: datetime | None = None
    created_at: datetime
    updated_at: datetime


class RunDetail(RunOut):
    """A run plus its config snapshot, checkpoint, and full event log."""

    config: dict[str, Any] | None = None
    checkpoint: dict[str, Any] | None = None
    events: list[RunEventOut] = []


class CancelRunResponse(BaseModel):
    """Result of a cancel request."""

    run_id: int
    cancelled: bool


# --- approval audit (Фаза 1.5 §2 — approval audit trail) ---


class ApprovalAuditOut(BaseModel):
    """One row of the approval audit trail."""

    id: int
    conversation_id: int
    run_id: int | None = None
    call_id: str
    tool_name: str
    arguments: dict[str, Any] | None = None
    approved: bool
    decision_source: str
    decided_by: str | None = None
    reason: str | None = None
    is_breakpoint: bool = False
    breakpoint_type: str | None = None
    duration_ms: int | None = None
    created_at: datetime


# --- artifacts (Фаза 1.5 §3 — artifacts & attachments) ---


class ArtifactOut(BaseModel):
    """Summary of a stored artifact."""

    id: int
    conversation_id: int
    run_id: int | None = None
    tool_call_id: str | None = None
    filename: str
    media_type: str
    kind: str
    size_bytes: int
    sha256: str | None = None
    version: int = 1
    parent_id: int | None = None
    metadata_: dict[str, Any] | None = None
    created_at: datetime
    updated_at: datetime


class ArtifactDetail(ArtifactOut):
    """Artifact with extracted text (for preview/context)."""

    extracted_text: str | None = None
    versions: list[ArtifactOut] = []


class ArtifactUploadResponse(BaseModel):
    """Result of a successful artifact upload."""

    artifact: ArtifactOut
    message: str = "uploaded"


# --- inspector (Фаза 1.5 §6 — Debug / Inspector Mode) ---


class IterationDetail(BaseModel):
    """Per-iteration detail reconstructed from the event log."""

    iteration: int
    duration_ms: int | None = None
    usage: dict[str, Any] | None = None
    model: str | None = None
    tool_calls: list[dict[str, Any]] = []
    finish_reason: str | None = None


class RunTimeline(BaseModel):
    """Full run timeline with per-iteration breakdown."""

    run: RunDetail
    iterations: list[IterationDetail] = []
    total_duration_ms: int | None = None


class RunComparison(BaseModel):
    """Side-by-side comparison of two runs."""

    run_a: RunOut
    run_b: RunOut
    delta_tokens: int
    delta_cost_usd: float | None = None
    delta_iterations: int
    delta_duration_ms: int | None = None
    iterations_a: list[IterationDetail] = []
    iterations_b: list[IterationDetail] = []


class ReplayRequest(BaseModel):
    """Request body for replaying a run with optional overrides."""

    model: str | None = None
    system_prompt: str | None = None
    temperature: float | None = None


class ReplayResponse(BaseModel):
    """Result of initiating a replay."""

    new_run_id: int
    original_run_id: int
    status: str


# --- planning mode (Фаза 2 §1) ---


class PlanStepOut(BaseModel):
    """One step within a plan."""

    position: int
    title: str
    description: str | None = None
    status: str = "pending"
    depends_on: list[int] | None = None
    tools: list[str] | None = None
    result_summary: str | None = None


class PlanOut(BaseModel):
    """Summary of a plan."""

    id: int
    conversation_id: int
    run_id: int | None = None
    title: str | None = None
    status: str
    steps: list[PlanStepOut] = []
    created_at: datetime
    updated_at: datetime


class PlanStepUpdate(BaseModel):
    """Editable step fields (for plan editing before approval)."""

    position: int
    title: str
    description: str | None = None
    depends_on: list[int] | None = None
    tools: list[str] | None = None


class PlanUpdate(BaseModel):
    """Edit a draft plan's title and/or steps."""

    title: str | None = None
    steps: list[PlanStepUpdate] | None = None


class PlanApproveRequest(BaseModel):
    """Approve or reject a draft plan."""

    approved: bool = True


class PlanTemplateOut(BaseModel):
    """A reusable plan template."""

    id: int
    name: str
    description: str | None = None
    steps: list[dict[str, Any]] = []
    is_builtin: bool = False
    created_at: datetime
    updated_at: datetime


class PlanTemplateCreate(BaseModel):
    """Create a new plan template."""

    name: str
    description: str | None = None
    steps: list[dict[str, Any]] = []


# --- subagents (Фаза 2 §5) ---


class SubagentRoleCreate(BaseModel):
    """Create a new subagent role definition."""

    name: str
    description: str | None = None
    system_prompt: str | None = None
    model: str | None = None
    tool_names: list[str] | None = None
    capability_policy: dict[str, str] | None = None
    max_iterations: int = 10
    max_cost_usd: float | None = None


class SubagentRoleUpdate(BaseModel):
    """Update an existing subagent role."""

    name: str | None = None
    description: str | None = None
    system_prompt: str | None = None
    model: str | None = None
    tool_names: list[str] | None = None
    capability_policy: dict[str, str] | None = None
    max_iterations: int | None = None
    max_cost_usd: float | None = None


class SubagentRoleOut(BaseModel):
    """A subagent role definition."""

    id: int
    name: str
    description: str | None = None
    system_prompt: str | None = None
    model: str | None = None
    tool_names: list[str] | None = None
    capability_policy: dict[str, str] | None = None
    max_iterations: int = 10
    max_cost_usd: float | None = None
    is_builtin: bool = False
    created_at: datetime
    updated_at: datetime


class SubagentLaunchRequest(BaseModel):
    """Launch a single subagent run."""

    prompt: str
    role_id: int | None = None
    parent_conversation_id: int
    name: str | None = None
    model: str | None = None


class SubagentLaunchBatchItem(BaseModel):
    """One item in a batch launch."""

    prompt: str
    role_id: int | None = None
    name: str | None = None
    model: str | None = None


class SubagentLaunchBatchRequest(BaseModel):
    """Launch multiple subagents simultaneously."""

    parent_conversation_id: int
    items: list[SubagentLaunchBatchItem]


class SubagentRunOut(BaseModel):
    """Summary of a subagent run."""

    id: int
    role_id: int | None = None
    parent_conversation_id: int
    parent_run_id: int | None = None
    conversation_id: int
    run_id: int | None = None
    name: str | None = None
    prompt: str
    status: str
    result_summary: str | None = None
    usage: dict[str, Any] | None = None
    error: str | None = None
    started_at: datetime
    finished_at: datetime | None = None
    created_at: datetime
    updated_at: datetime


class SubagentRunDetail(SubagentRunOut):
    """A subagent run with its conversation messages."""

    messages: list[MessageOut] = []
