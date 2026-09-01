use std::fs;
use std::path::{Path, PathBuf};

use cool_protocol::*;
use schemars::schema_for;
use serde_json::{Value, json};
use ts_rs::{Config, TS};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let check = std::env::args()
        .skip(1)
        .any(|argument| argument == "--check");
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .ok_or("cool-protocol must live under <repo>/crates")?
        .to_path_buf();
    let schema_path = repository.join("schemas/cool-protocol-v1.schema.json");
    let typescript_path = repository.join("frontend/src/api/generated/cool_protocol.ts");
    let sdk_typescript_path = repository.join("sdk/typescript/src/generated/cool_protocol.ts");
    let golden_path = repository.join("crates/cool-protocol/tests/golden");

    let schema = serde_json::to_string_pretty(&schema_for!(ProtocolSchemaDocument))? + "\n";
    let typescript = generated_typescript();
    update_artifact(&schema_path, &schema, check)?;
    update_artifact(&typescript_path, &typescript, check)?;
    update_artifact(&sdk_typescript_path, &typescript, check)?;
    update_goldens(&golden_path, check)?;
    Ok(())
}

fn event(seq: u64, kind: &str, payload: Value) -> EventEnvelope {
    serde_json::from_value(json!({
        "eventId": format!("event-{seq}"),
        "schemaVersion": 1,
        "sessionId": "session-1",
        "runId": "run-1",
        "itemId": null,
        "seq": seq,
        "occurredAt": format!("2026-08-31T00:00:{seq:02}Z"),
        "actor": {"id": "local-user", "kind": "local_user"},
        "source": "golden",
        "causationId": null,
        "correlationId": null,
        "event": {"kind": kind, "payload": payload}
    }))
    .expect("generator scenarios must conform to the Rust protocol")
}

fn trace(name: &str, events: Vec<EventEnvelope>) -> GoldenTrace {
    let mut expected_state = ClientState::default();
    expected_state.last_seq = events.iter().map(|event| event.seq).max();
    match name {
        "chat" => {
            expected_state.run_status = Some("completed".to_owned());
            expected_state.content = "Hello world".to_owned();
        }
        "parallel-tools" => {
            expected_state.run_status = Some("completed".to_owned());
            expected_state
                .tools
                .insert("a".to_owned(), "completed".to_owned());
            expected_state
                .tools
                .insert("b".to_owned(), "completed".to_owned());
        }
        "approval-breakpoint" => {
            expected_state
                .tools
                .insert("write-1".to_owned(), "completed".to_owned());
            expected_state
                .approvals
                .insert("approval-1".to_owned(), "approved".to_owned());
        }
        "cancel-reconnect" => {
            expected_state.run_status = Some("cancelled".to_owned());
            expected_state.content = "once".to_owned();
        }
        "subagent" => {
            expected_state
                .subagents
                .insert("sub-1".to_owned(), "completed".to_owned());
        }
        "multimodal-artifact" => {
            expected_state.content = "See the chart.".to_owned();
            expected_state.artifacts.push("image-1".to_owned());
        }
        "budget" => expected_state.budget_status = Some("exceeded".to_owned()),
        "research" => expected_state.research_status = Some("completed".to_owned()),
        "worker-crash" => {
            expected_state.run_status = Some("completed".to_owned());
            expected_state
                .workers
                .insert("worker-1".to_owned(), "running".to_owned());
        }
        "error" => {
            expected_state.run_status = Some("failed".to_owned());
            expected_state
                .tools
                .insert("call-1".to_owned(), "failed".to_owned());
        }
        "plan" => {
            expected_state.active_plan_id = Some("plan-1".to_owned());
            expected_state.plan_status = Some("completed".to_owned());
            expected_state
                .plan_steps
                .insert("1".to_owned(), "completed".to_owned());
            expected_state.plan_completed_steps = 1;
            expected_state.plan_total_steps = 1;
        }
        "plan-failed" => {
            expected_state.active_plan_id = Some("plan-failed".to_owned());
            expected_state.plan_status = Some("failed".to_owned());
            expected_state
                .plan_steps
                .insert("1".to_owned(), "failed".to_owned());
            expected_state.plan_completed_steps = 0;
            expected_state.plan_total_steps = 1;
        }
        _ => panic!("missing independent golden oracle for {name}"),
    }
    GoldenTrace {
        name: name.to_owned(),
        events,
        expected_state,
    }
}

fn golden_traces() -> Vec<GoldenTrace> {
    vec![
        trace(
            "chat",
            vec![
                event(
                    1,
                    "run.started",
                    json!({"model": "scripted", "mode": "chat"}),
                ),
                event(
                    2,
                    "content.delta",
                    json!({"text": "Hello", "channel": "final"}),
                ),
                event(
                    3,
                    "content.delta",
                    json!({"text": " world", "channel": "final"}),
                ),
                event(
                    4,
                    "run.completed",
                    json!({"reason": "stop", "errorCode": null}),
                ),
            ],
        ),
        trace(
            "parallel-tools",
            vec![
                event(1, "run.started", json!({"model": null, "mode": "agent"})),
                event(
                    2,
                    "tool.requested",
                    json!({"callId": "a", "name": "read_file", "arguments": {"path": "a.txt"}}),
                ),
                event(
                    3,
                    "tool.requested",
                    json!({"callId": "b", "name": "read_file", "arguments": {"path": "b.txt"}}),
                ),
                event(
                    4,
                    "tool.started",
                    json!({"callId": "a", "name": "read_file"}),
                ),
                event(
                    5,
                    "tool.started",
                    json!({"callId": "b", "name": "read_file"}),
                ),
                event(
                    6,
                    "tool.completed",
                    json!({"callId": "b", "name": "read_file", "result": "B"}),
                ),
                event(
                    7,
                    "tool.completed",
                    json!({"callId": "a", "name": "read_file", "result": "A"}),
                ),
                event(
                    8,
                    "run.completed",
                    json!({"reason": "stop", "errorCode": null}),
                ),
            ],
        ),
        trace(
            "approval-breakpoint",
            vec![
                event(
                    1,
                    "tool.requested",
                    json!({"callId": "write-1", "name": "write_file", "arguments": {"path": "note.txt"}}),
                ),
                event(
                    2,
                    "tool.approval_required",
                    json!({
                        "callId": "write-1", "name": "write_file", "arguments": {"path": "note.txt"},
                        "reason": "write capability", "approvalId": "approval-1", "revision": 1,
                        "breakpointType": "before_write", "resultPreview": null, "currentContent": "old"
                    }),
                ),
                event(
                    3,
                    "tool.approval_resolved",
                    json!({"callId": "write-1", "approvalId": "approval-1", "revision": 1, "decision": "approved"}),
                ),
                event(
                    4,
                    "tool.started",
                    json!({"callId": "write-1", "name": "write_file"}),
                ),
                event(
                    5,
                    "tool.completed",
                    json!({"callId": "write-1", "name": "write_file", "result": {"written": true}}),
                ),
            ],
        ),
        {
            let first = event(1, "run.started", json!({"model": null, "mode": "chat"}));
            let delta = event(
                2,
                "content.delta",
                json!({"text": "once", "channel": "final"}),
            );
            trace(
                "cancel-reconnect",
                vec![
                    first,
                    delta.clone(),
                    delta,
                    event(
                        3,
                        "run.cancelled",
                        json!({"reason": "user", "errorCode": null}),
                    ),
                ],
            )
        },
        trace(
            "plan",
            vec![
                event(
                    1,
                    "plan.created",
                    json!({"planId": "plan-1", "title": "Ship", "totalSteps": 1}),
                ),
                event(
                    2,
                    "plan.step_started",
                    json!({"planId": "plan-1", "position": 1, "title": "Test", "status": "running", "resultSummary": null}),
                ),
                event(
                    3,
                    "plan.progress",
                    json!({"planId": "plan-1", "completedSteps": 0, "totalSteps": 1, "message": "testing", "status": "executing"}),
                ),
                event(
                    4,
                    "plan.step_completed",
                    json!({"planId": "plan-1", "position": 1, "title": "Test", "status": "completed", "resultSummary": "ok"}),
                ),
                event(
                    5,
                    "plan.progress",
                    json!({"planId": "plan-1", "completedSteps": 1, "totalSteps": 1, "message": null, "status": "completed"}),
                ),
            ],
        ),
        trace(
            "plan-failed",
            vec![
                event(
                    1,
                    "plan.created",
                    json!({"planId": "plan-failed", "title": "Fail", "totalSteps": 1}),
                ),
                event(
                    2,
                    "plan.step_started",
                    json!({"planId": "plan-failed", "position": 1, "title": "Break", "status": "running", "resultSummary": null}),
                ),
                event(
                    3,
                    "plan.step_completed",
                    json!({"planId": "plan-failed", "position": 1, "title": "Break", "status": "failed", "resultSummary": "boom"}),
                ),
                event(
                    4,
                    "plan.progress",
                    json!({"planId": "plan-failed", "completedSteps": 0, "totalSteps": 1, "message": null, "status": "failed"}),
                ),
            ],
        ),
        trace(
            "subagent",
            vec![
                event(
                    1,
                    "subagent.started",
                    json!({"subagentRunId": "sub-1", "name": "reviewer", "status": "running", "summary": null, "error": null}),
                ),
                event(
                    2,
                    "subagent.progress",
                    json!({"subagentRunId": "sub-1", "message": "checking", "percent": 50.0}),
                ),
                event(
                    3,
                    "subagent.completed",
                    json!({"subagentRunId": "sub-1", "name": "reviewer", "status": "completed", "summary": "clean", "error": null}),
                ),
            ],
        ),
        trace(
            "multimodal-artifact",
            vec![
                event(
                    1,
                    "artifact.created",
                    json!({"artifactId": "image-1", "kind": "image", "name": "chart.png", "mediaType": "image/png"}),
                ),
                event(
                    2,
                    "content.delta",
                    json!({"text": "See the chart.", "channel": "final"}),
                ),
            ],
        ),
        trace(
            "budget",
            vec![
                event(
                    1,
                    "budget.warning",
                    json!({"window": "daily", "spendUsd": 8.0, "limitUsd": 10.0, "percent": 80.0}),
                ),
                event(
                    2,
                    "budget.exceeded",
                    json!({"window": "daily", "spendUsd": 10.1, "limitUsd": 10.0, "percent": 101.0}),
                ),
            ],
        ),
        trace(
            "research",
            vec![
                event(
                    1,
                    "research.started",
                    json!({"researchRunId": "research-1"}),
                ),
                event(
                    2,
                    "research.stage",
                    json!({"stage": "gather", "message": null, "progress": 0.25}),
                ),
                event(
                    3,
                    "research.source_found",
                    json!({"url": "https://example.com", "title": "Primary", "snippet": "Evidence", "confidence": 0.9}),
                ),
                event(
                    4,
                    "research.subquestion_started",
                    json!({"index": 0, "question": "Why?", "status": "running"}),
                ),
                event(
                    5,
                    "research.subquestion_completed",
                    json!({"index": 0, "question": "Why?", "status": "completed"}),
                ),
                event(
                    6,
                    "research.completed",
                    json!({"artifactId": "report-1", "sourceCount": 1, "error": null}),
                ),
            ],
        ),
        trace(
            "worker-crash",
            vec![
                event(
                    1,
                    "worker.started",
                    json!({"workerId": "worker-1", "attempt": 1, "code": null}),
                ),
                event(
                    2,
                    "worker.failed",
                    json!({"workerId": "worker-1", "attempt": 1, "code": "process_exit"}),
                ),
                event(
                    3,
                    "worker.restarted",
                    json!({"workerId": "worker-1", "attempt": 2, "code": null}),
                ),
                event(
                    4,
                    "run.completed",
                    json!({"reason": "recovered", "errorCode": null}),
                ),
            ],
        ),
        trace(
            "error",
            vec![
                event(1, "run.started", json!({"model": null, "mode": "agent"})),
                event(
                    2,
                    "tool.failed",
                    json!({"callId": "call-1", "name": "bash", "errorCode": "sandbox_denied", "message": "denied"}),
                ),
                event(
                    3,
                    "run.failed",
                    json!({"reason": "tool failed", "errorCode": "sandbox_denied"}),
                ),
            ],
        ),
    ]
}

fn update_goldens(directory: &Path, check: bool) -> Result<(), Box<dyn std::error::Error>> {
    let traces = golden_traces();
    let expected_names = traces
        .iter()
        .map(|trace| format!("{}.json", trace.name))
        .collect::<std::collections::BTreeSet<_>>();
    if check {
        let actual_names = fs::read_dir(directory)?
            .filter_map(Result::ok)
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| name.ends_with(".json"))
            .collect::<std::collections::BTreeSet<_>>();
        if actual_names != expected_names {
            return Err("golden trace file set drift; run the generator".into());
        }
    }
    for trace in traces {
        let content = serde_json::to_string_pretty(&trace)? + "\n";
        update_artifact(
            &directory.join(format!("{}.json", trace.name)),
            &content,
            check,
        )?;
    }
    Ok(())
}

fn generated_typescript() -> String {
    let mut output = String::from(
        "// @generated by `cargo run -p cool-protocol --bin generate`; do not edit.\n\n",
    );
    output.push_str(
        "export type JsonValue = null | boolean | number | string | JsonValue[] | { [key: string]: JsonValue };\n\n",
    );
    let config = Config::default();
    macro_rules! declarations {
        ($($type:ty),+ $(,)?) => {
            $(
                output.push_str("export ");
                output.push_str(&<$type as TS>::decl(&config));
                output.push_str("\n\n");
            )+
        };
    }
    declarations!(
        ActorRef,
        ActorKind,
        CommandEnvelope,
        Command,
        InitializeParams,
        SessionCreateParams,
        SessionLoadParams,
        SessionPromptParams,
        ContentPart,
        RunCancelParams,
        RunEventsParams,
        ApprovalResolveParams,
        ApprovalDecision,
        ApprovalOutcome,
        EventCursor,
        EventPage,
        ProtocolError,
        EventEnvelope,
        CanonicalEvent,
        SessionEvent,
        SessionCompacted,
        RunStarted,
        RunTerminal,
        ItemEvent,
        TextDelta,
        ToolRequested,
        ToolApprovalRequired,
        ToolApprovalResolved,
        ToolLifecycle,
        ToolCompleted,
        ToolFailed,
        PlanCreated,
        PlanStep,
        PlanProgressStatus,
        PlanProgress,
        ArtifactCreated,
        UsageUpdated,
        BudgetEvent,
        SubagentEvent,
        SubagentProgress,
        WorkerEvent,
        ResearchStage,
        ResearchStarted,
        ResearchSource,
        ResearchSubquestion,
        ResearchTerminal,
        ClientState,
        GoldenTrace,
        StreamFrame,
        StreamKeepalive,
        StreamEnd,
        JsonRpcV2,
        CoolCommandMethod,
        RunEventMethod,
        RpcId,
        RpcRequest,
        TransportLimits,
        InitializeResult,
        SessionCreatedResult,
        SessionLoadedResult,
        PromptAcceptedResult,
        RunCancelledResult,
        ApprovalResolvedResult,
        ResponsePayload,
        RpcSuccess,
        RpcFailure,
        RpcNotification,
        ServerFrame,
    );
    output.truncate(output.trim_end_matches('\n').len());
    output.push('\n');
    output
}

fn update_artifact(
    path: &Path,
    expected: &str,
    check: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if check {
        let actual = fs::read_to_string(path).map_err(|error| {
            format!("generated artifact missing at {}: {error}", path.display())
        })?;
        if actual != expected {
            return Err(format!(
                "generated artifact drift at {}; run the generator",
                path.display()
            )
            .into());
        }
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, expected)?;
    println!("generated {}", path.display());
    Ok(())
}
