use std::collections::BTreeSet;
use std::sync::Arc;

use cool_agent::{
    AgentLimits, AgentRequest, AgentRuntime, AutoApprovalGate, CancelSignal, ModelEvent,
    ScriptedDriver, StoreEventSink, ToolCall, ToolContext, builtin_registry,
};
use cool_protocol::{ApprovalOutcome, CanonicalEvent};
use cool_security::{Capability, CapabilityPolicy, Decision, PolicyRule, Workspace};
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
    #[serde(default)]
    rules: Vec<PolicyRule>,
    /// Process launcher to attach (`"host"`); unset keeps the fail-closed
    /// `DisabledLauncher` default.
    launcher: Option<String>,
    approval: String,
    expected_events: Vec<String>,
    expected_file: bool,
    /// Substring that must appear in the `tool.completed` result payload.
    expected_output_contains: Option<String>,
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
        if !scenario.rules.is_empty() {
            policy.set_rules(scenario.rules);
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
        let mut tool_context = ToolContext::new(Workspace::new(directory.path()).unwrap(), policy);
        if let Some(launcher) = scenario.launcher.as_deref() {
            tool_context = match launcher {
                "host" => {
                    // Host scenarios spawn the real `git` binary — skip on
                    // a runner that cannot resolve it on PATH.
                    if !git_on_path() {
                        eprintln!("scenario {} skipped: git not on PATH", scenario.id);
                        continue;
                    }
                    tool_context
                        .with_launcher(Arc::new(cool_agent::HostLauncher))
                        .with_environment(std::env::vars().collect())
                }
                other => panic!("unknown launcher {other}"),
            };
        }
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
                    user_parts: Vec::new(),
                    user_replay_parts: Vec::new(),
                    system_prompt: None,
                    mode: None,
                    temperature: 0.0,
                    max_tokens: None,
                    limits: AgentLimits::default(),
                    tool_names: Some(BTreeSet::from([
                        "read_file".to_owned(),
                        "write_file".to_owned(),
                        "edit_file".to_owned(),
                        "search_files".to_owned(),
                        "find_files".to_owned(),
                        "shell".to_owned(),
                        "view_image".to_owned(),
                    ])),
                    tool_context,
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
        if let Some(needle) = scenario.expected_output_contains {
            let payload = events
                .iter()
                .find_map(|event| match &event.event {
                    CanonicalEvent::ToolCompleted(completed) => Some(completed.result.to_string()),
                    _ => None,
                })
                .unwrap_or_else(|| {
                    panic!("scenario {} expected a tool.completed event", scenario.id)
                });
            assert!(
                payload.contains(&needle),
                "scenario {} tool output must contain {needle:?}, got {payload}",
                scenario.id
            );
        }
        store.replay_run(&run, "local-user").unwrap();
    }
}

fn git_on_path() -> bool {
    std::process::Command::new("git")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn parse_capability(value: &str) -> Capability {
    match value {
        "read" => Capability::Read,
        "write" => Capability::Write,
        "network" => Capability::Network,
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
