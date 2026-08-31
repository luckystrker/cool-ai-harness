use std::fs;
use std::path::PathBuf;

use cool_protocol::{
    ClientState, CommandEnvelope, ContentPart, EventEnvelope, GoldenTrace, StreamFrame,
};
use serde_json::json;

fn golden_files() -> Vec<PathBuf> {
    let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden");
    let mut files = fs::read_dir(directory)
        .expect("golden directory")
        .map(|entry| entry.expect("golden entry").path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect::<Vec<_>>();
    files.sort();
    files
}

#[test]
fn every_golden_trace_deserializes_and_replays_deterministically() {
    let files = golden_files();
    assert_eq!(files.len(), 12, "M1 requires all critical scenarios");

    for file in files {
        let json = fs::read_to_string(&file).expect("read golden trace");
        let trace: GoldenTrace = serde_json::from_str(&json).expect("valid protocol trace");
        assert_eq!(
            serde_json::to_value(ClientState::replay(&trace.events)).expect("serialize state"),
            serde_json::to_value(&trace.expected_state).expect("serialize expected state"),
            "Rust reducer drift in {}",
            file.display()
        );

        let roundtrip = serde_json::to_string(&trace).expect("serialize trace");
        let reparsed: GoldenTrace = serde_json::from_str(&roundtrip).expect("roundtrip trace");
        assert_eq!(reparsed, trace, "lossy trace in {}", file.display());
    }
}

#[test]
fn duplicate_event_ids_are_idempotent() {
    let json = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/cancel-reconnect.json"),
    )
    .expect("read reconnect trace");
    let trace: GoldenTrace = serde_json::from_str(&json).expect("valid reconnect trace");
    let deltas = trace
        .events
        .iter()
        .filter(|event| matches!(event.event, cool_protocol::CanonicalEvent::ContentDelta(_)))
        .collect::<Vec<&EventEnvelope>>();
    assert_eq!(deltas.len(), 2, "fixture must include a replayed event");
    assert_eq!(deltas[0].event_id, deltas[1].event_id);
    assert_eq!(trace.expected_state.content, "once");
}

#[test]
fn replay_uses_sequence_order_and_rejects_gaps_and_collisions() {
    let json = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/cancel-reconnect.json"),
    )
    .expect("read reconnect trace");
    let trace: GoldenTrace = serde_json::from_str(&json).expect("valid reconnect trace");
    let mut permuted = trace.events.clone();
    permuted.reverse();
    assert_eq!(
        serde_json::to_value(ClientState::try_replay(&permuted).expect("ordered by seq"))
            .expect("state value"),
        serde_json::to_value(&trace.expected_state).expect("expected value")
    );

    let gap = trace
        .events
        .iter()
        .filter(|event| event.seq != 2)
        .cloned()
        .collect::<Vec<_>>();
    assert!(ClientState::try_replay(&gap).is_err());

    let mut collision = trace.events.clone();
    collision[2].event = cool_protocol::CanonicalEvent::ContentDelta(cool_protocol::TextDelta {
        text: "different".to_owned(),
        channel: Some("final".to_owned()),
    });
    assert!(ClientState::try_replay(&collision).is_err());
}

#[test]
fn replay_rejects_events_for_a_different_plan() {
    let json = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/plan.json"),
    )
    .expect("read plan trace");
    let mut trace: GoldenTrace = serde_json::from_str(&json).expect("valid plan trace");
    let progress = trace
        .events
        .iter_mut()
        .find_map(|event| match &mut event.event {
            cool_protocol::CanonicalEvent::PlanProgress(payload) => Some(payload),
            _ => None,
        })
        .expect("plan progress fixture");
    progress.plan_id = "other-plan".to_owned();

    let error = ClientState::try_replay(&trace.events).expect_err("mismatch must fail");
    assert!(error.to_string().contains("plan id mismatch"));
}

#[test]
fn v1_wire_types_reject_client_identity_wrong_versions_and_unknown_fields() {
    let valid_read = json!({
        "protocolVersion": 1,
        "commandId": "command-1",
        "command": {"method": "session.load", "params": {"sessionId": "session-1"}}
    });
    assert!(serde_json::from_value::<CommandEnvelope>(valid_read.clone()).is_ok());

    let mut forged_actor = valid_read.clone();
    forged_actor["actor"] = json!({"id": "admin", "kind": "server_user"});
    assert!(serde_json::from_value::<CommandEnvelope>(forged_actor).is_err());
    let mut rogue_command = valid_read.clone();
    rogue_command["command"]["rogue"] = json!(true);
    assert!(serde_json::from_value::<CommandEnvelope>(rogue_command).is_err());

    let mut wrong_protocol = valid_read;
    wrong_protocol["protocolVersion"] = json!(2);
    assert!(serde_json::from_value::<CommandEnvelope>(wrong_protocol).is_err());

    let missing_idempotency = json!({
        "protocolVersion": 1,
        "commandId": "command-2",
        "command": {
            "method": "session.create",
            "params": {"title": null, "projectKey": null}
        }
    });
    assert!(serde_json::from_value::<CommandEnvelope>(missing_idempotency).is_err());
    let empty_idempotency = json!({
        "protocolVersion": 1,
        "commandId": "command-3",
        "command": {
            "method": "run.cancel",
            "params": {"idempotencyKey": "", "runId": "run-1", "reason": null}
        }
    });
    assert!(serde_json::from_value::<CommandEnvelope>(empty_idempotency).is_err());

    let trace_json = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/chat.json"),
    )
    .expect("read chat trace");
    let trace: GoldenTrace = serde_json::from_str(&trace_json).expect("trace");
    let mut envelope = serde_json::to_value(&trace.events[0]).expect("event value");
    envelope["rogue"] = json!(true);
    assert!(serde_json::from_value::<EventEnvelope>(envelope.clone()).is_err());
    envelope
        .as_object_mut()
        .expect("event object")
        .remove("rogue");
    envelope["event"]["rogue"] = json!(true);
    assert!(serde_json::from_value::<EventEnvelope>(envelope.clone()).is_err());
    envelope["event"]
        .as_object_mut()
        .expect("canonical event object")
        .remove("rogue");
    envelope["schemaVersion"] = json!(2);
    assert!(serde_json::from_value::<EventEnvelope>(envelope).is_err());

    assert!(
        serde_json::from_value::<ContentPart>(json!({
            "type": "text", "text": "hello", "rogue": true
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<StreamFrame>(json!({
            "type": "keepalive",
            "value": {"runId": "run-1", "lastSeq": 1},
            "rogue": true
        }))
        .is_err()
    );
}
