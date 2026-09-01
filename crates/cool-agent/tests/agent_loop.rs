use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use cool_agent::{
    AgentLimits, AgentRequest, AgentRuntime, AutoApprovalGate, CancelSignal, EventSink, Message,
    ModelEvent, RunOutcome, ScriptedDriver, StoreEventSink, SubagentRequest, Tool, ToolContext,
    ToolDefinition, ToolError, ToolHandler, ToolRegistry, ToolResult, Usage, builtin_registry,
};
use cool_protocol::{
    ActorKind, ActorRef, ApprovalOutcome, CanonicalEvent, EventEnvelope, V1Version,
};
use cool_security::{CapabilityPolicy, Decision, Workspace};
use cool_state::{BudgetLimits, DurableStore};
use serde_json::{Map, json};
use tempfile::tempdir;

struct SlowTool;

#[async_trait]
impl ToolHandler for SlowTool {
    async fn execute(
        &self,
        _context: &ToolContext,
        _arguments: serde_json::Value,
    ) -> Result<ToolResult, ToolError> {
        tokio::time::sleep(Duration::from_secs(10)).await;
        Ok(ToolResult::ok(json!("late")))
    }
}

#[derive(Default)]
struct RecordingSink {
    events: Mutex<Vec<EventEnvelope>>,
}

impl RecordingSink {
    fn kinds(&self) -> Vec<&'static str> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .map(|event| match event.event {
                CanonicalEvent::RunStarted(_) => "run.started",
                CanonicalEvent::ContentDelta(_) => "content.delta",
                CanonicalEvent::UsageUpdated(_) => "usage.updated",
                CanonicalEvent::ToolRequested(_) => "tool.requested",
                CanonicalEvent::ToolApprovalRequired(_) => "tool.approval_required",
                CanonicalEvent::ToolApprovalResolved(_) => "tool.approval_resolved",
                CanonicalEvent::ToolStarted(_) => "tool.started",
                CanonicalEvent::ToolCompleted(_) => "tool.completed",
                CanonicalEvent::ToolFailed(_) => "tool.failed",
                CanonicalEvent::RunCompleted(_) => "run.completed",
                CanonicalEvent::RunCancelled(_) => "run.cancelled",
                CanonicalEvent::RunFailed(_) => "run.failed",
                CanonicalEvent::ItemCompleted(_) => "item.completed",
                CanonicalEvent::PlanCreated(_) => "plan.created",
                CanonicalEvent::PlanStepStarted(_) => "plan.step_started",
                CanonicalEvent::PlanStepCompleted(_) => "plan.step_completed",
                CanonicalEvent::PlanProgress(_) => "plan.progress",
                _ => "other",
            })
            .collect()
    }
}

#[async_trait]
impl EventSink for RecordingSink {
    async fn emit(&self, event: CanonicalEvent) -> Result<EventEnvelope, cool_agent::RuntimeError> {
        let mut events = self.events.lock().unwrap();
        let envelope = EventEnvelope {
            event_id: format!("event-{}", events.len() + 1),
            schema_version: V1Version::VALUE,
            session_id: "session".to_owned(),
            run_id: "run".to_owned(),
            item_id: None,
            seq: events.len() as u64 + 1,
            occurred_at: "test".to_owned(),
            actor: ActorRef {
                id: "cool-agent".to_owned(),
                kind: ActorKind::System,
            },
            source: "test".to_owned(),
            causation_id: None,
            correlation_id: None,
            event,
            extensions: BTreeMap::new(),
        };
        events.push(envelope.clone());
        Ok(envelope)
    }
}

fn request(workspace: &std::path::Path) -> AgentRequest {
    AgentRequest {
        model: "scripted".to_owned(),
        history: Vec::new(),
        user_input: "hello".to_owned(),
        system_prompt: Some("be precise".to_owned()),
        temperature: 0.0,
        max_tokens: None,
        limits: AgentLimits::default(),
        tool_names: None,
        tool_context: ToolContext::new(
            Workspace::new(workspace).unwrap(),
            CapabilityPolicy::new(Some(Decision::Allow)),
        ),
    }
}

#[tokio::test]
async fn scripted_chat_streams_usage_and_completes() {
    let directory = tempdir().unwrap();
    let provider = ScriptedDriver::new([Ok(vec![
        ModelEvent::Content("hel".to_owned()),
        ModelEvent::Content("lo".to_owned()),
        ModelEvent::Usage(Usage {
            prompt_tokens: 2,
            completion_tokens: 1,
            total_tokens: 3,
            cost_micro_usd: Some(7),
        }),
        ModelEvent::Finish {
            reason: Some("stop".to_owned()),
        },
    ])]);
    let runtime = AgentRuntime::new(Arc::new(provider), builtin_registry());
    let sink = RecordingSink::default();
    let (_, cancel) = CancelSignal::channel();
    let outcome = runtime
        .run(
            request(directory.path()),
            &sink,
            &AutoApprovalGate {
                outcome: ApprovalOutcome::Approved,
            },
            cancel,
        )
        .await
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Completed { .. }));
    assert_eq!(
        sink.kinds(),
        [
            "run.started",
            "item.completed",
            "content.delta",
            "content.delta",
            "usage.updated",
            "item.completed",
            "run.completed"
        ]
    );
}

#[tokio::test]
async fn approval_tool_loop_writes_then_returns_to_the_provider() {
    let directory = tempdir().unwrap();
    let provider = ScriptedDriver::new([
        Ok(vec![
            ModelEvent::ToolCall(cool_agent::ToolCall {
                call_id: "call-write".to_owned(),
                name: "write_file".to_owned(),
                arguments: Map::from_iter([
                    ("path".to_owned(), json!("result.txt")),
                    ("content".to_owned(), json!("durable")),
                ]),
            }),
            ModelEvent::Finish {
                reason: Some("tool_calls".to_owned()),
            },
        ]),
        Ok(vec![
            ModelEvent::Content("done".to_owned()),
            ModelEvent::Finish {
                reason: Some("stop".to_owned()),
            },
        ]),
    ]);
    let runtime = AgentRuntime::new(Arc::new(provider), builtin_registry());
    let sink = RecordingSink::default();
    let (_, cancel) = CancelSignal::channel();
    let outcome = runtime
        .run(
            request(directory.path()),
            &sink,
            &AutoApprovalGate {
                outcome: ApprovalOutcome::Approved,
            },
            cancel,
        )
        .await
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Completed { .. }));
    assert_eq!(
        std::fs::read_to_string(directory.path().join("result.txt")).unwrap(),
        "durable"
    );
    let kinds = sink.kinds();
    for expected in [
        "tool.requested",
        "tool.approval_required",
        "tool.approval_resolved",
        "tool.started",
        "tool.completed",
        "run.completed",
    ] {
        assert!(kinds.contains(&expected), "missing {expected}: {kinds:?}");
    }
}

#[tokio::test]
async fn parallel_batch_records_partial_failure_and_keeps_valid_history() {
    let directory = tempdir().unwrap();
    std::fs::write(directory.path().join("ok.txt"), "ok").unwrap();
    let provider = ScriptedDriver::new([
        Ok(vec![
            ModelEvent::ToolCall(cool_agent::ToolCall {
                call_id: "good".to_owned(),
                name: "read_file".to_owned(),
                arguments: Map::from_iter([("path".to_owned(), json!("ok.txt"))]),
            }),
            ModelEvent::ToolCall(cool_agent::ToolCall {
                call_id: "bad".to_owned(),
                name: "read_file".to_owned(),
                arguments: Map::from_iter([("path".to_owned(), json!("missing.txt"))]),
            }),
            ModelEvent::Finish {
                reason: Some("tool_calls".to_owned()),
            },
        ]),
        Ok(vec![ModelEvent::Finish {
            reason: Some("stop".to_owned()),
        }]),
    ]);
    let runtime = AgentRuntime::new(Arc::new(provider), builtin_registry());
    let sink = RecordingSink::default();
    let (_, cancel) = CancelSignal::channel();
    let outcome = runtime
        .run(
            request(directory.path()),
            &sink,
            &AutoApprovalGate {
                outcome: ApprovalOutcome::Approved,
            },
            cancel,
        )
        .await
        .unwrap();
    let RunOutcome::Completed { history, .. } = outcome else {
        panic!("expected completion")
    };
    let tool_messages = history
        .iter()
        .filter(|message| message.role == cool_agent::MessageRole::Tool)
        .collect::<Vec<_>>();
    assert_eq!(tool_messages.len(), 2);
    assert_eq!(tool_messages[0].tool_call_id.as_deref(), Some("good"));
    assert_eq!(tool_messages[1].tool_call_id.as_deref(), Some("bad"));
    let kinds = sink.kinds();
    assert_eq!(
        kinds
            .iter()
            .filter(|kind| **kind == "tool.completed")
            .count(),
        1
    );
    assert_eq!(
        kinds.iter().filter(|kind| **kind == "tool.failed").count(),
        1
    );
}

#[tokio::test]
async fn retryable_provider_failure_retries_before_any_visible_delta() {
    let directory = tempdir().unwrap();
    let provider = ScriptedDriver::new([
        Err(cool_agent::ProviderError::new("temporary", "retry", true)),
        Ok(vec![
            ModelEvent::Content("recovered".to_owned()),
            ModelEvent::Finish { reason: None },
        ]),
    ]);
    let runtime = AgentRuntime::new(Arc::new(provider), builtin_registry());
    let sink = RecordingSink::default();
    let (_, cancel) = CancelSignal::channel();
    assert!(matches!(
        runtime
            .run(
                request(directory.path()),
                &sink,
                &AutoApprovalGate {
                    outcome: ApprovalOutcome::Approved,
                },
                cancel,
            )
            .await
            .unwrap(),
        RunOutcome::Completed { .. }
    ));
}

#[tokio::test]
async fn planning_tool_emits_replayable_plan_progress() {
    let directory = tempdir().unwrap();
    let provider = ScriptedDriver::new([
        Ok(vec![
            ModelEvent::ToolCall(cool_agent::ToolCall {
                call_id: "plan-call".to_owned(),
                name: "update_plan".to_owned(),
                arguments: Map::from_iter([
                    ("planId".to_owned(), json!("plan-1")),
                    ("title".to_owned(), json!("M7")),
                    (
                        "steps".to_owned(),
                        json!([
                            {"title":"done","status":"completed"},
                            {"title":"active","status":"in_progress"},
                            {"title":"later","status":"pending"}
                        ]),
                    ),
                ]),
            }),
            ModelEvent::Finish {
                reason: Some("tool_calls".to_owned()),
            },
        ]),
        Ok(vec![ModelEvent::Finish { reason: None }]),
    ]);
    let runtime = AgentRuntime::new(Arc::new(provider), builtin_registry());
    let sink = RecordingSink::default();
    let (_, cancel) = CancelSignal::channel();
    runtime
        .run(
            request(directory.path()),
            &sink,
            &AutoApprovalGate {
                outcome: ApprovalOutcome::Approved,
            },
            cancel,
        )
        .await
        .unwrap();
    let kinds = sink.kinds();
    for expected in [
        "plan.created",
        "plan.step_completed",
        "plan.step_started",
        "plan.progress",
    ] {
        assert!(kinds.contains(&expected), "missing {expected}: {kinds:?}");
    }
}

#[tokio::test]
async fn cancellation_aborts_parallel_tools_and_closes_every_requested_call() {
    let directory = tempdir().unwrap();
    let provider = ScriptedDriver::new([Ok(vec![
        ModelEvent::ToolCall(cool_agent::ToolCall {
            call_id: "slow-a".to_owned(),
            name: "slow".to_owned(),
            arguments: Map::new(),
        }),
        ModelEvent::ToolCall(cool_agent::ToolCall {
            call_id: "slow-b".to_owned(),
            name: "slow".to_owned(),
            arguments: Map::new(),
        }),
        ModelEvent::Finish {
            reason: Some("tool_calls".to_owned()),
        },
    ])]);
    let registry = ToolRegistry::new([Tool::new(
        ToolDefinition {
            name: "slow".to_owned(),
            description: "slow deterministic fixture".to_owned(),
            parameters: json!({"type":"object"}),
        },
        [],
        Decision::Allow,
        SlowTool,
    )])
    .unwrap();
    let runtime = AgentRuntime::new(Arc::new(provider), registry);
    let sink = Arc::new(RecordingSink::default());
    let (cancel_sender, cancel) = CancelSignal::channel();
    let path = directory.path().to_owned();
    let run_sink = sink.clone();
    let task = tokio::spawn(async move {
        runtime
            .run(
                request(&path),
                run_sink.as_ref(),
                &AutoApprovalGate {
                    outcome: ApprovalOutcome::Approved,
                },
                cancel,
            )
            .await
            .unwrap()
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    cancel_sender.send(Some("test_cancel".to_owned())).unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .expect("cancelled batch must stop promptly")
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Cancelled { .. }));
    let kinds = sink.kinds();
    assert_eq!(
        kinds
            .iter()
            .filter(|kind| **kind == "tool.requested")
            .count(),
        2
    );
    assert_eq!(
        kinds.iter().filter(|kind| **kind == "tool.failed").count(),
        2
    );
    assert_eq!(kinds.last(), Some(&"run.cancelled"));
}

#[tokio::test]
async fn durable_sink_replay_matches_the_terminal_projection() {
    let directory = tempdir().unwrap();
    let store = DurableStore::in_memory().unwrap();
    let session = store
        .create_session("local-user", "session-key", "session-fp", None, None)
        .unwrap()
        .value;
    let run = store
        .start_run("local-user", "run-key", "run-fp", &session)
        .unwrap()
        .value;
    let sink = StoreEventSink::new(store.clone(), "local-user", &session, &run);
    let runtime = AgentRuntime::new(Arc::new(ScriptedDriver::echo()), builtin_registry());
    let (_, cancel) = CancelSignal::channel();
    runtime
        .run(
            request(directory.path()),
            &sink,
            &AutoApprovalGate {
                outcome: ApprovalOutcome::Approved,
            },
            cancel,
        )
        .await
        .unwrap();
    let replay = store.replay_run(&run, "local-user").unwrap();
    assert_eq!(replay.status, cool_state::RunStatus::Completed);
}

#[tokio::test]
async fn durable_sink_masks_user_content_and_tool_arguments() {
    let directory = tempdir().unwrap();
    let store = DurableStore::in_memory().unwrap();
    let session = store
        .create_session("local-user", "secret-session", "secret-session", None, None)
        .unwrap()
        .value;
    let run = store
        .start_run("local-user", "secret-run", "secret-run", &session)
        .unwrap()
        .value;
    let sink = StoreEventSink::new(store.clone(), "local-user", &session, &run);
    let provider = ScriptedDriver::new([
        Ok(vec![
            ModelEvent::ToolCall(cool_agent::ToolCall {
                call_id: "secret-call".to_owned(),
                name: "missing_tool".to_owned(),
                arguments: Map::from_iter([("api_key".to_owned(), json!("short-secret"))]),
            }),
            ModelEvent::Finish {
                reason: Some("tool_calls".to_owned()),
            },
        ]),
        Ok(vec![ModelEvent::Finish {
            reason: Some("stop".to_owned()),
        }]),
    ]);
    let runtime = AgentRuntime::new(Arc::new(provider), builtin_registry());
    let (_, cancel) = CancelSignal::channel();
    let mut agent_request = request(directory.path());
    agent_request.user_input = "Authorization: Bearer abcdefghijklmnopqrstuvwxyz123456".to_owned();
    runtime
        .run(
            agent_request,
            &sink,
            &AutoApprovalGate {
                outcome: ApprovalOutcome::Approved,
            },
            cancel,
        )
        .await
        .unwrap();
    let encoded = serde_json::to_string(&store.all_events(&run, "local-user").unwrap()).unwrap();
    assert!(!encoded.contains("abcdefghijklmnopqrstuvwxyz123456"));
    assert!(!encoded.contains("short-secret"));
    assert!(encoded.contains("[REDACTED]"));
}

#[tokio::test]
async fn atomic_budget_rejection_closes_the_run_before_another_provider_call() {
    let directory = tempdir().unwrap();
    let store = DurableStore::in_memory().unwrap();
    store
        .set_budget_limits(
            "local-user",
            "run-budget",
            BudgetLimits {
                tokens: Some(1),
                ..BudgetLimits::default()
            },
        )
        .unwrap();
    let session = store
        .create_session("local-user", "budget-session", "budget-session", None, None)
        .unwrap()
        .value;
    let run = store
        .start_run("local-user", "budget-run", "budget-run", &session)
        .unwrap()
        .value;
    let sink = StoreEventSink::new(store.clone(), "local-user", &session, &run)
        .with_budget_window("run-budget");
    let provider = ScriptedDriver::new([Ok(vec![ModelEvent::Usage(Usage {
        prompt_tokens: 2,
        completion_tokens: 0,
        total_tokens: 2,
        cost_micro_usd: None,
    })])]);
    let runtime = AgentRuntime::new(Arc::new(provider), builtin_registry());
    let (_, cancel) = CancelSignal::channel();
    let outcome = runtime
        .run(
            request(directory.path()),
            &sink,
            &AutoApprovalGate {
                outcome: ApprovalOutcome::Approved,
            },
            cancel,
        )
        .await
        .unwrap();
    assert!(matches!(
        outcome,
        RunOutcome::Failed { ref code, .. } if code == "run_budget_exceeded"
    ));
    assert_eq!(
        store.replay_run(&run, "local-user").unwrap().status,
        cool_state::RunStatus::Failed
    );
}

#[tokio::test]
async fn configured_usage_limit_fails_closed_when_a_later_turn_omits_usage() {
    let directory = tempdir().unwrap();
    let provider = ScriptedDriver::new([
        Ok(vec![
            ModelEvent::Usage(Usage {
                prompt_tokens: 1,
                completion_tokens: 0,
                total_tokens: 1,
                cost_micro_usd: None,
            }),
            ModelEvent::ToolCall(cool_agent::ToolCall {
                call_id: "missing-read".to_owned(),
                name: "read_file".to_owned(),
                arguments: Map::from_iter([("path".to_owned(), json!("missing.txt"))]),
            }),
            ModelEvent::Finish {
                reason: Some("tool_calls".to_owned()),
            },
        ]),
        Ok(vec![ModelEvent::Finish {
            reason: Some("stop".to_owned()),
        }]),
    ]);
    let runtime = AgentRuntime::new(Arc::new(provider), builtin_registry());
    let sink = RecordingSink::default();
    let (_, cancel) = CancelSignal::channel();
    let mut agent_request = request(directory.path());
    agent_request.limits.max_total_tokens = Some(100);
    let outcome = runtime
        .run(
            agent_request,
            &sink,
            &AutoApprovalGate {
                outcome: ApprovalOutcome::Approved,
            },
            cancel,
        )
        .await
        .unwrap();
    assert!(matches!(
        outcome,
        RunOutcome::Failed { ref code, .. } if code == "run_budget_exceeded"
    ));
}

#[tokio::test]
async fn configured_cost_limit_fails_closed_when_a_later_turn_has_unknown_cost() {
    let directory = tempdir().unwrap();
    let provider = ScriptedDriver::new([
        Ok(vec![
            ModelEvent::Usage(Usage {
                prompt_tokens: 1,
                completion_tokens: 0,
                total_tokens: 1,
                cost_micro_usd: Some(1),
            }),
            ModelEvent::ToolCall(cool_agent::ToolCall {
                call_id: "cost-read".to_owned(),
                name: "read_file".to_owned(),
                arguments: Map::from_iter([("path".to_owned(), json!("missing.txt"))]),
            }),
            ModelEvent::Finish {
                reason: Some("tool_calls".to_owned()),
            },
        ]),
        Ok(vec![
            ModelEvent::Usage(Usage {
                prompt_tokens: 2,
                completion_tokens: 0,
                total_tokens: 2,
                cost_micro_usd: None,
            }),
            ModelEvent::Finish {
                reason: Some("stop".to_owned()),
            },
        ]),
    ]);
    let runtime = AgentRuntime::new(Arc::new(provider), builtin_registry());
    let sink = RecordingSink::default();
    let (_, cancel) = CancelSignal::channel();
    let mut agent_request = request(directory.path());
    agent_request.limits.max_cost_micro_usd = Some(100);
    let outcome = runtime
        .run(
            agent_request,
            &sink,
            &AutoApprovalGate {
                outcome: ApprovalOutcome::Approved,
            },
            cancel,
        )
        .await
        .unwrap();
    assert!(matches!(
        outcome,
        RunOutcome::Failed { ref code, .. } if code == "run_budget_exceeded"
    ));
}

#[tokio::test]
async fn a_new_run_loads_the_prior_durable_session_history() {
    let directory = tempdir().unwrap();
    let store = DurableStore::in_memory().unwrap();
    let session = store
        .create_session(
            "local-user",
            "session-history",
            "session-history",
            None,
            None,
        )
        .unwrap()
        .value;
    let driver = ScriptedDriver::new([
        Ok(vec![
            ModelEvent::Content("first answer".to_owned()),
            ModelEvent::Finish { reason: None },
        ]),
        Ok(vec![
            ModelEvent::Content("second answer".to_owned()),
            ModelEvent::Finish { reason: None },
        ]),
    ]);
    let runtime = AgentRuntime::new(Arc::new(driver.clone()), builtin_registry());
    for (index, prompt) in ["first question", "second question"]
        .into_iter()
        .enumerate()
    {
        let run = store
            .start_run(
                "local-user",
                &format!("run-{index}"),
                &format!("run-{index}"),
                &session,
            )
            .unwrap()
            .value;
        let sink = StoreEventSink::new(store.clone(), "local-user", &session, run);
        let (_, cancel) = CancelSignal::channel();
        let mut agent_request = request(directory.path());
        agent_request.user_input = prompt.to_owned();
        runtime
            .run(
                agent_request,
                &sink,
                &AutoApprovalGate {
                    outcome: ApprovalOutcome::Approved,
                },
                cancel,
            )
            .await
            .unwrap();
    }
    let requests = driver.requests().await;
    let second = &requests[1].messages;
    let transcript = second
        .iter()
        .filter_map(|message| message.content.as_deref())
        .collect::<Vec<_>>();
    assert!(transcript.contains(&"first question"));
    assert!(transcript.contains(&"first answer"));
    assert!(transcript.contains(&"second question"));
}

#[tokio::test]
async fn subagent_uses_a_separate_durable_run_and_reports_to_the_parent() {
    let directory = tempdir().unwrap();
    let store = DurableStore::in_memory().unwrap();
    let parent_session = store
        .create_session("local-user", "parent-session", "parent-session", None, None)
        .unwrap()
        .value;
    let parent_run = store
        .start_run("local-user", "parent-run", "parent-run", &parent_session)
        .unwrap()
        .value;
    let child_session = store
        .create_session("local-user", "child-session", "child-session", None, None)
        .unwrap()
        .value;
    let child_run = store
        .start_run("local-user", "child-run", "child-run", &child_session)
        .unwrap()
        .value;
    let parent_sink =
        StoreEventSink::new(store.clone(), "local-user", &parent_session, &parent_run);
    let child_sink = StoreEventSink::new(store.clone(), "local-user", &child_session, &child_run);
    let runtime = AgentRuntime::new(Arc::new(ScriptedDriver::echo()), builtin_registry());
    let (_, cancel) = CancelSignal::channel();
    let outcome = runtime
        .run_subagent(
            SubagentRequest {
                run_id: child_run.clone(),
                role: "reviewer".to_owned(),
                agent: request(directory.path()),
                cancel,
            },
            &parent_sink,
            &child_sink,
            &AutoApprovalGate {
                outcome: ApprovalOutcome::Approved,
            },
        )
        .await
        .unwrap();
    assert!(matches!(outcome, RunOutcome::Completed { .. }));
    assert_eq!(
        store.replay_run(&child_run, "local-user").unwrap().status,
        cool_state::RunStatus::Completed
    );
    let parent_events = store.all_events(&parent_run, "local-user").unwrap();
    assert!(matches!(
        parent_events[0].event,
        CanonicalEvent::SubagentStarted(_)
    ));
    assert!(matches!(
        parent_events[1].event,
        CanonicalEvent::SubagentCompleted(_)
    ));
}

#[test]
fn project_instructions_and_compaction_keep_security_and_tool_groups() {
    let directory = tempdir().unwrap();
    std::fs::write(directory.path().join("AGENTS.md"), "project rule").unwrap();
    let loaded = cool_agent::load_project_instructions(&Workspace::new(directory.path()).unwrap())
        .unwrap()
        .unwrap();
    assert!(loaded.contains("cannot override security policies"));
    assert!(loaded.contains("project rule"));
    std::fs::write(directory.path().join("AGENTS.md"), "x".repeat(20_000)).unwrap();
    let bounded = cool_agent::load_project_instructions(&Workspace::new(directory.path()).unwrap())
        .unwrap()
        .unwrap();
    assert!(bounded.contains("truncated"));
    assert!(bounded.len() < 17_000);

    let call = cool_agent::ToolCall {
        call_id: "call".to_owned(),
        name: "read_file".to_owned(),
        arguments: Map::new(),
    };
    let history = vec![
        Message::text(cool_agent::MessageRole::System, "system"),
        Message::text(cool_agent::MessageRole::User, "old".repeat(100)),
        Message {
            role: cool_agent::MessageRole::Assistant,
            content: None,
            tool_calls: vec![call.clone()],
            tool_call_id: None,
            name: None,
        },
        Message::tool_result(&call, "result"),
    ];
    let compacted = cool_agent::compact_history(&history, 35);
    assert!(compacted.dropped_messages > 0);
    assert_eq!(compacted.messages[0].role, cool_agent::MessageRole::System);
    assert_eq!(
        compacted.messages[1].role,
        cool_agent::MessageRole::Assistant
    );
    assert_eq!(compacted.messages[2].role, cool_agent::MessageRole::Tool);
}
