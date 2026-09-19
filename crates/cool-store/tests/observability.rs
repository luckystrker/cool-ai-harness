//! Inspector projection tests (M10): timeline reconstruction from the legacy
//! `run_events` log, phase grouping, durations, status mapping, cross-run
//! comparison and actor isolation.
//!
//! The parity test replays the same seed used to generate
//! `tests/fixtures/timeline_parity.json` with the real Python inspector
//! (`backend/app/observability/inspector.py`). Python groups the seq-ordered
//! events into per-iteration records; Rust exposes a flat seq-ordered entry
//! list, so the assertion reconstructs the Python iteration view from the
//! `llm_call_complete` entries (normalization documented below).

mod common;

use common::{DEFAULT_SEED, create_python_database, temp_database};
use cool_store::domains::runs::{NewRun, NewToolCall};
use cool_store::observability::{RunTimeline, compare_runs, run_timeline};
use cool_store::{LegacyStore, StoreError};
use serde_json::Value;
use tempfile::TempDir;

/// Committed output of the real Python inspector over the shared seed.
const PARITY_FIXTURE: &str = include_str!("fixtures/timeline_parity.json");

fn adopted_store() -> (TempDir, LegacyStore) {
    let directory = TempDir::new().expect("tempdir");
    let path = temp_database(directory.path(), "harness.db");
    create_python_database(&path, DEFAULT_SEED);
    let store = LegacyStore::open(
        &path,
        &cool_store::StoreOptions {
            initialize_if_missing: false,
            ..cool_store::StoreOptions::default()
        },
    )
    .expect("adopt");
    (directory, store)
}

/// Normalize an RFC3339 fixture timestamp to the SQLAlchemy representation.
fn sql_timestamp(value: &str) -> String {
    let seconds = cool_store::parse_python_datetime(value).expect("fixture timestamp");
    cool_store::python_datetime(seconds, 0)
}

fn set_run_times(store: &LegacyStore, run_id: i64, started: &str, finished: &str) {
    let started = sql_timestamp(started);
    let finished = sql_timestamp(finished);
    store
        .with_connection(|connection| {
            connection.execute(
                "UPDATE agent_runs SET started_at = ?1, finished_at = ?2 WHERE id = ?3",
                rusqlite::params![started, finished, run_id],
            )?;
            Ok(())
        })
        .expect("set run times");
}

/// Seed `run_a`/`run_b` from the parity fixture through the typed store API.
fn seed_run_from_fixture(store: &LegacyStore, run_key: &str, events_key: &str) -> i64 {
    let fixture: Value = serde_json::from_str(PARITY_FIXTURE).expect("fixture json");
    let seed = &fixture["seed"];
    let run = &seed[run_key];

    let created = store
        .create_run(
            "local-user",
            1,
            &NewRun {
                model: Some("m".to_string()),
                config: None,
            },
        )
        .expect("create run");

    for event in seed[events_key].as_array().expect("events array") {
        let kind = event[0].as_str().expect("event kind");
        let payload = &event[1];
        let payload = (!payload.is_null()).then_some(payload);
        store
            .append_run_event("local-user", created.id, kind, payload)
            .expect("append event");
    }

    store
        .finish_run(
            "local-user",
            created.id,
            run["status"].as_str().expect("run status"),
            Some(&run["usage"]),
            Some(run["iterations"].as_i64().expect("iterations")),
            run["finish_reason"].as_str(),
            run["error"].as_str(),
        )
        .expect("finish run");

    set_run_times(
        store,
        created.id,
        run["started_at"].as_str().expect("started_at"),
        run["finished_at"].as_str().expect("finished_at"),
    );
    created.id
}

/// Assert the Rust projection reproduces Python's `TimelineResult`.
///
/// Normalization: Python emits one `IterationInfo` per `llm_call_complete`
/// event (with tool calls folded into the preceding iteration). Rust emits one
/// entry per event, so iterations are reconstructed from the `llm` entries and
/// tool calls from the flat `tool_call_start` entries.
fn assert_timeline_matches_python(timeline: &RunTimeline, python: &Value) {
    assert_eq!(
        timeline.total_duration_ms,
        python["total_duration_ms"].as_i64(),
        "event-derived total must match Python"
    );

    let iterations = python["iterations"].as_array().expect("iterations");
    let llm_entries: Vec<&cool_store::observability::TimelineEntry> = timeline
        .entries
        .iter()
        .filter(|entry| entry.kind == "llm_call_complete")
        .collect();
    assert_eq!(llm_entries.len(), iterations.len(), "iteration count");

    for (entry, iteration) in llm_entries.iter().zip(iterations) {
        let payload = entry.payload.as_ref().expect("llm payload");
        assert_eq!(
            payload["iteration"].as_i64(),
            iteration["iteration"].as_i64()
        );
        assert_eq!(entry.duration_ms, iteration["duration_ms"].as_i64());
        assert_eq!(payload["model"], iteration["model"]);
        assert_eq!(payload["usage"], iteration["usage"]);
    }

    let rust_tools: Vec<&str> = timeline
        .entries
        .iter()
        .filter(|entry| entry.kind == "tool_call_start")
        .filter_map(|entry| entry.payload.as_ref())
        .filter_map(|payload| payload.get("name"))
        .filter_map(Value::as_str)
        .collect();
    let python_tools: Vec<&str> = iterations
        .iter()
        .filter_map(|iteration| iteration["tool_calls"].as_array())
        .flatten()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert_eq!(rust_tools, python_tools, "tool-call order/names");

    let python_finish_reason = iterations
        .last()
        .and_then(|iteration| iteration["finish_reason"].as_str());
    let rust_finish_reason = timeline
        .entries
        .iter()
        .rev()
        .find(|entry| entry.kind == "finish")
        .and_then(|entry| entry.payload.as_ref())
        .and_then(|payload| payload.get("reason"))
        .and_then(Value::as_str);
    assert_eq!(rust_finish_reason, python_finish_reason, "finish reason");
}

#[test]
fn timeline_entries_follow_event_sequence_and_group_into_phases() {
    let (_directory, store) = adopted_store();
    let run_id = seed_run_from_fixture(&store, "run_a", "events_a");
    let timeline = run_timeline(&store, "local-user", run_id).expect("timeline");

    let fixture: Value = serde_json::from_str(PARITY_FIXTURE).expect("fixture json");
    let expected_kinds: Vec<&str> = fixture["seed"]["events_a"]
        .as_array()
        .expect("events")
        .iter()
        .map(|event| event[0].as_str().expect("kind"))
        .collect();
    let kinds: Vec<&str> = timeline
        .entries
        .iter()
        .map(|entry| entry.kind.as_str())
        .collect();
    assert_eq!(kinds, expected_kinds, "entries must follow seq order");

    for (position, entry) in timeline.entries.iter().enumerate() {
        assert_eq!(entry.index, position);
        assert!(!entry.occurred_at.is_empty(), "occurred_at is preserved");
    }

    let phases: Vec<&str> = timeline
        .entries
        .iter()
        .map(|entry| entry.phase.as_str())
        .collect();
    assert_eq!(
        phases,
        vec![
            "start", "llm", "message", "tools", "tools", "llm", "message", "finalize"
        ]
    );
    assert_eq!(
        timeline
            .entries
            .iter()
            .filter(|entry| entry.phase == "llm")
            .count(),
        2
    );
    assert_eq!(
        timeline
            .entries
            .iter()
            .filter(|entry| entry.phase == "tools")
            .count(),
        2
    );

    assert_eq!(timeline.status, "completed");
    assert_eq!(
        timeline.started_at.as_deref(),
        Some("2026-02-01 10:00:00.000000")
    );
    assert_eq!(
        timeline.finished_at.as_deref(),
        Some("2026-02-01 10:00:01.000000")
    );
    assert_eq!(
        timeline
            .usage
            .as_ref()
            .and_then(|usage| usage["total_tokens"].as_i64()),
        Some(100)
    );
    assert!(timeline.error.is_none());
}

#[test]
fn timeline_computes_durations_and_maps_statuses() {
    let (_directory, store) = adopted_store();
    let run_a = seed_run_from_fixture(&store, "run_a", "events_a");
    let run_b = seed_run_from_fixture(&store, "run_b", "events_b");

    let timeline_a = run_timeline(&store, "local-user", run_a).expect("run a timeline");
    assert_eq!(timeline_a.total_duration_ms, Some(250));
    let llm_durations: Vec<Option<i64>> = timeline_a
        .entries
        .iter()
        .filter(|entry| entry.kind == "llm_call_complete")
        .map(|entry| entry.duration_ms)
        .collect();
    assert_eq!(llm_durations, vec![Some(100), Some(150)]);

    let tool = timeline_a
        .entries
        .iter()
        .find(|entry| entry.kind == "tool_result")
        .expect("tool result entry");
    assert_eq!(tool.duration_ms, Some(12));
    assert_eq!(tool.status.as_deref(), Some("completed"));
    assert_eq!(tool.title.as_deref(), Some("read_file"));

    let finish = timeline_a
        .entries
        .iter()
        .find(|entry| entry.kind == "finish")
        .expect("finish entry");
    assert_eq!(finish.duration_ms, Some(250));
    assert_eq!(finish.status.as_deref(), Some("completed"));
    assert_eq!(finish.title.as_deref(), Some("stop"));

    let timeline_b = run_timeline(&store, "local-user", run_b).expect("run b timeline");
    assert_eq!(timeline_b.status, "failed");
    assert_eq!(timeline_b.total_duration_ms, Some(220));
    assert_eq!(timeline_b.error.as_deref(), Some("boom"));

    let error = timeline_b
        .entries
        .iter()
        .find(|entry| entry.kind == "error")
        .expect("error entry");
    assert_eq!(error.phase, "finalize");
    assert_eq!(error.status.as_deref(), Some("failed"));
    assert_eq!(error.title.as_deref(), Some("boom"));

    let finish_b = timeline_b
        .entries
        .iter()
        .find(|entry| entry.kind == "finish")
        .expect("finish entry");
    assert_eq!(finish_b.status.as_deref(), Some("failed"));
}

#[test]
fn timeline_duration_falls_back_to_finish_elapsed_and_handles_empty_runs() {
    let (_directory, store) = adopted_store();

    let empty = store
        .create_run("local-user", 1, &NewRun::default())
        .expect("empty run");
    let timeline = run_timeline(&store, "local-user", empty.id).expect("timeline");
    assert!(timeline.entries.is_empty());
    assert_eq!(timeline.total_duration_ms, None);
    assert_eq!(timeline.status, "running");

    let fallback = store
        .create_run("local-user", 1, &NewRun::default())
        .expect("fallback run");
    store
        .append_run_event(
            "local-user",
            fallback.id,
            "finish",
            Some(&serde_json::json!({"reason": "stop", "elapsed_ms": 500})),
        )
        .expect("finish event");
    let timeline = run_timeline(&store, "local-user", fallback.id).expect("timeline");
    assert_eq!(timeline.total_duration_ms, Some(500));
}

#[test]
fn timeline_only_reads_run_events_not_messages_tool_calls_or_approvals() {
    let (_directory, store) = adopted_store();
    let run_id = seed_run_from_fixture(&store, "run_a", "events_a");

    store
        .record_tool_call(
            "local-user",
            &NewToolCall {
                conversation_id: Some(1),
                message_id: None,
                name: "not_in_the_log".to_string(),
                arguments: None,
                result: None,
                duration_ms: Some(999),
                success: true,
                error: None,
            },
        )
        .expect("record tool call");

    let timeline = run_timeline(&store, "local-user", run_id).expect("timeline");
    assert!(
        timeline
            .entries
            .iter()
            .all(|entry| entry.title.as_deref() != Some("not_in_the_log")),
        "tool_calls rows must not be merged into the event-log timeline"
    );
}

#[test]
fn timeline_and_comparison_match_python_inspector_fixture() {
    let (_directory, store) = adopted_store();
    let fixture: Value = serde_json::from_str(PARITY_FIXTURE).expect("fixture json");
    let run_a = seed_run_from_fixture(&store, "run_a", "events_a");
    let run_b = seed_run_from_fixture(&store, "run_b", "events_b");

    assert_timeline_matches_python(
        &run_timeline(&store, "local-user", run_a).expect("run a timeline"),
        &fixture["timeline_a"],
    );
    assert_timeline_matches_python(
        &run_timeline(&store, "local-user", run_b).expect("run b timeline"),
        &fixture["timeline_b"],
    );

    let comparison = compare_runs(&store, "local-user", run_a, run_b).expect("comparison");
    let python = &fixture["comparison"];
    assert_eq!(
        comparison.deltas.total_tokens,
        python["delta_tokens"].as_i64()
    );
    assert_eq!(
        comparison.deltas.duration_ms,
        python["delta_duration_ms"].as_i64()
    );
    assert_eq!(
        comparison.deltas.cost_usd,
        python["delta_cost_usd"].as_f64()
    );
    // Rust-only deltas are `right - left` (run_b has 5 events vs run_a's 8,
    // and run_b has no `tool_call_start` while run_a has one).
    assert_eq!(comparison.deltas.event_count, -3);
    assert_eq!(comparison.deltas.tool_calls, -1);
    assert_eq!(comparison.left.run_id, run_a);
    assert_eq!(comparison.right.run_id, run_b);
}

#[test]
fn inspector_reads_are_actor_scoped() {
    let (_directory, store) = adopted_store();
    let run_id = seed_run_from_fixture(&store, "run_a", "events_a");

    store.ensure_actor("other-actor").expect("register actor");
    store
        .create_conversation("other-actor", &Default::default())
        .expect("other actor conversation");

    let error = run_timeline(&store, "other-actor", run_id).expect_err("cross-actor timeline");
    assert!(matches!(error, StoreError::NotFound(_)));

    let error =
        compare_runs(&store, "other-actor", run_id, run_id).expect_err("cross-actor comparison");
    assert!(matches!(error, StoreError::NotFound(_)));
}
