# M1 client-visible stream inventory

This inventory is derived from the current producers and transport routes. It is the reviewable
bridge between legacy Python tags and App Protocol v1; adapter coverage tests additionally compare
the `EventKind` literal with the implementation map.

## AgentEvent

| Legacy kind | Canonical kind | Producer/context notes |
|---|---|---|
| `start` | `run.started` | Runner binds conversation/run/actor before any client serialization. |
| `thinking`, `react_thought` | `reasoning.delta` | Legacy tag retained as canonical channel. |
| `token` | `content.delta` | Final assistant text stream. |
| `tool_call_start`, `tool_call_delta`, `react_action` | `tool.requested` | Full exact legacy delta remains in the namespaced extension. |
| `tool_approval_request` | `tool.approval_required` | Executor registers the request before yielding it and includes a server-generated approval ID, revision, breakpoint type/current content/result preview. |
| `tool_approval_resolved` | `tool.approval_resolved` | Approved, denied or timed-out terminal state for the same approval ID/revision. |
| `tool_result` | `tool.completed` or `tool.failed` | Selected from the masked result's error fields. |
| `message` | `item.completed` | Persisted assistant message; exact tool calls/thinking remain in extension during compatibility. |
| `finish` | `run.completed` | Terminal reason retained. |
| `error` | `run.failed` | Safe public message maps to stable `agent_error`. |
| `budget_alert` | `budget.warning` | Window, USD values and percentage use canonical units. |
| `react_observation` | `item.updated` | Tool observation summary. |
| `llm_call_complete` | `usage.updated` | OpenAI and Anthropic token-key aliases normalize to one usage shape. |
| `plan_generated` | `plan.created` | Captures real persisted plan ID; generation and execution streams emit it before progress. |
| `plan_step_start`, `plan_step_complete` | `plan.step_started`, `plan.step_completed` | Constructors require the real plan ID. |
| `plan_progress` | `plan.progress` | Constructors require the real plan ID, step counts and explicit executing/completed/failed status. |
| `subagent_started` | `subagent.started` | Real subagent run ID. |
| `subagent_progress` | `subagent.progress` | Iteration/content update. |
| `subagent_completed`, `subagent_failed` | `subagent.completed`, `subagent.failed` | Terminal summary/error. |

`run_conversation_turn` owns one adapter for the real durable run. It binds monotonically sequenced
canonical envelopes to events before SSE/WebSocket or inspector projection. Direct executor-only
events are intentionally unbound and cannot claim canonical identity.

## Research

| Legacy type | Canonical kind | Actual producer fields |
|---|---|---|
| `started` | `research.started` | API route: `run_id`. |
| `stage` | `research.stage` | Orchestrator: `stage`; optional message/progress accepted. |
| `source_found` | `research.source_found` | Orchestrator: `url`, `title`; optional snippet/confidence accepted. |
| `subquestion_started` | `research.subquestion_started` | `index`, `sub_question`. |
| `subquestion_completed` | `research.subquestion_completed` | `index`, optional `sub_question`, `status`. |
| `completed` | `research.completed` | `run_id`, `report_artifact_id`, `sources_count`, `citations_count`. |
| `failed` | `research.failed` | `run_id`, `error`. |
| `cancelled` | `research.cancelled` | `run_id`. |

The API constructs one `EventSink`/adapter per research run and sends `started` through that same
path. Canonical typed payloads use the real report artifact and source count field names.

## Inspector, subagent and transport frames

- The inspector registry receives the runner-bound event's legacy projection; therefore its event
  sequence has already passed the canonical adapter even though the current endpoint still emits
  `{kind,payload}`.
- Subagent live streams forward those inspector events and use the same mapping above.
- Inspector/subagent keepalive and end-of-stream are transport lifecycle frames, represented by
  `StreamFrame::Keepalive` and `StreamFrame::End`; they are not durable events and never enter the
  reducer.
- A WebSocket error produced before a run exists is a transport error, not a canonical run event.
- Approval responses are commands (`approval.resolve` with required idempotency key); approval and
  breakpoint requests/resolutions are the `tool.approval_required` and
  `tool.approval_resolved` events above. The compatibility endpoint accepts only the
  server-generated approval ID plus the expected revision/run ID and verifies actor, conversation
  and run ownership. Model-provided call IDs are never global registry keys.
- Plan execution emits `plan.created` before progress. Both reducers bind subsequent plan events
  to that ID, reject mixed-plan state mutation and use the explicit terminal status instead of
  inferring success from counters.
