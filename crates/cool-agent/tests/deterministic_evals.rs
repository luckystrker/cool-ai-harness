use std::collections::BTreeSet;
use std::sync::Arc;

use cool_agent::{
    AgentLimits, AgentRequest, AgentRuntime, AutoApprovalGate, CancelSignal, ModelEvent,
    ScriptedDriver, StoreEventSink, ToolCall, ToolContext, builtin_registry,
};
use cool_protocol::{ApprovalOutcome, CanonicalEvent};
use cool_security::{Capability, CapabilityPolicy, Decision, Workspace};
use cool_state::DurableStore;
use serde::Deserialize;
use serde_json::{Map, Value};
use tempfile::tempdir;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Scenario {
    id: String,
    calls: Vec<FixtureCall>,
    capability: Option<String>,
    decision: Option<String>,
    approval: String,
    expected_events: Vec<String>,
    expected_file: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FixtureCall {
    call_id: String,
    name: String,
    arguments: Map<String, Value>,
}

#[tokio::test]
async fn critical_deterministic_scenarios_pass_on_the_rust_runtime() {
    let scenarios: Vec<Scenario> =
        serde_json::from_str(include_str!("fixtures/evals.json")).unwrap();
    for scenario in scenarios {
        let directory = tempdir().unwrap();
        let mut policy = CapabilityPolicy::new(Some(Decision::Allow));
        if let (Some(capability), Some(decision)) = (&scenario.capability, &scenario.decision) {
            policy.set(parse_capability(capability), parse_decision(decision));
        }
        let calls = scenario
            .calls
            .into_iter()
            .map(|call| {
                ModelEvent::ToolCall(ToolCall {
                    call_id: call.call_id,
                    name: call.name,
                    arguments: call.arguments,
                })
            })
            .chain(std::iter::once(ModelEvent::Finish {
                reason: Some("tool_calls".to_owned()),
            }))
            .collect::<Vec<_>>();
        let provider = ScriptedDriver::new([
            Ok(calls),
            Ok(vec![
                ModelEvent::Content("eval complete".to_owned()),
                ModelEvent::Finish { reason: None },
            ]),
        ]);
        let store = DurableStore::in_memory().unwrap();
        let session = store
            .create_session("local-user", "session", "session", None, None)
            .unwrap()
            .value;
        let run = store
            .start_run("local-user", "run", "run", &session)
            .unwrap()
            .value;
        let sink = StoreEventSink::new(store.clone(), "local-user", &session, &run);
        let (_, cancel) = CancelSignal::channel();
        AgentRuntime::new(Arc::new(provider), builtin_registry())
            .run(
                AgentRequest {
                    model: "scripted".to_owned(),
                    history: Vec::new(),
                    user_input: scenario.id.clone(),
                    system_prompt: None,
                    temperature: 0.0,
                    max_tokens: None,
                    limits: AgentLimits::default(),
                    tool_names: Some(BTreeSet::from([
                        "read_file".to_owned(),
                        "write_file".to_owned(),
                    ])),
                    tool_context: ToolContext::new(
                        Workspace::new(directory.path()).unwrap(),
                        policy,
                    ),
                },
                &sink,
                &AutoApprovalGate {
                    outcome: match scenario.approval.as_str() {
                        "approved" => ApprovalOutcome::Approved,
                        "denied" => ApprovalOutcome::Denied,
                        other => panic!("unknown approval {other}"),
                    },
                },
                cancel,
            )
            .await
            .unwrap();
        let events = store.all_events(&run, "local-user").unwrap();
        let kinds = events
            .iter()
            .map(|event| event_kind(&event.event))
            .collect::<Vec<_>>();
        let observed = kinds
            .into_iter()
            .filter(|kind| *kind != "other")
            .collect::<Vec<_>>();
        assert_eq!(
            observed, scenario.expected_events,
            "scenario {} canonical event sequence mismatch",
            scenario.id
        );
        assert_eq!(
            directory.path().join("eval.txt").exists(),
            scenario.expected_file,
            "scenario {} file side effect mismatch",
            scenario.id
        );
        store.replay_run(&run, "local-user").unwrap();
    }
}

fn parse_capability(value: &str) -> Capability {
    match value {
        "read" => Capability::Read,
        "write" => Capability::Write,
        other => panic!("unknown capability {other}"),
    }
}

fn parse_decision(value: &str) -> Decision {
    match value {
        "allow" => Decision::Allow,
        "ask" => Decision::Ask,
        "deny" => Decision::Deny,
        other => panic!("unknown decision {other}"),
    }
}

fn event_kind(event: &CanonicalEvent) -> &'static str {
    match event {
        CanonicalEvent::ToolApprovalRequired(_) => "tool.approval_required",
        CanonicalEvent::ToolApprovalResolved(_) => "tool.approval_resolved",
        CanonicalEvent::ToolCompleted(_) => "tool.completed",
        CanonicalEvent::ToolFailed(_) => "tool.failed",
        CanonicalEvent::RunCompleted(_) => "run.completed",
        _ => "other",
    }
}
