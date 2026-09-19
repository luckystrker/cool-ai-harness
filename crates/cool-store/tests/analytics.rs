//! Analytics aggregation parity tests.

mod common;

use common::{DEFAULT_SEED, create_python_database, temp_database};
use cool_store::{LegacyStore, StoreError};
use std::collections::BTreeMap;
use tempfile::TempDir;

/// A large lookback so the fixed 2026-01 fixtures are always in range.
const DAYS: i64 = 3_650;

const ANALYTICS_SEED: &str = r#"
INSERT INTO agent_runs(created_at, updated_at, id, conversation_id, user_id, status, iterations, started_at)
VALUES ('2026-01-01 09:00:00.000000', '2026-01-01 09:00:00.000000', 1, 1, 1, 'completed', 2, '2026-01-01 09:00:00.000000');
INSERT INTO run_events(created_at, updated_at, id, run_id, seq, kind, payload) VALUES
 ('2026-01-01 10:00:00.000000', '2026-01-01 10:00:00.000000', 1, 1, 1, 'llm_call_complete', '{"duration_ms": 100}'),
 ('2026-01-01 10:05:00.000000', '2026-01-01 10:05:00.000000', 2, 1, 2, 'llm_call_complete', '{"duration_ms": 300}');
INSERT INTO spend_log(created_at, updated_at, id, user_id, provider_name, model, prompt_tokens, completion_tokens, total_tokens, cost_usd, ts) VALUES
 ('2026-01-01 10:00:00.000000', '2026-01-01 10:00:00.000000', 1, 1, 'openai', 'gpt-4o', 100, 50, 150, 0.5, '2026-01-01 10:00:00.000000'),
 ('2026-01-01 11:00:00.000000', '2026-01-01 11:00:00.000000', 2, 1, 'openai', 'gpt-4o', 200, 100, 300, 1.0, '2026-01-01 11:00:00.000000'),
 ('2026-01-02 09:00:00.000000', '2026-01-02 09:00:00.000000', 3, 1, 'anthropic', 'claude-3', 10, 5, 15, 0.25, '2026-01-02 09:00:00.000000');
INSERT INTO tool_calls(created_at, updated_at, id, conversation_id, user_id, name, success, duration_ms) VALUES
 ('2026-01-01 10:00:00.000000', '2026-01-01 10:00:00.000000', 1, 1, 1, 'read_file', 1, 10),
 ('2026-01-01 10:01:00.000000', '2026-01-01 10:01:00.000000', 2, 1, 1, 'read_file', 0, 20),
 ('2026-01-01 10:02:00.000000', '2026-01-01 10:02:00.000000', 3, 1, 1, 'write_file', 1, 30);
INSERT INTO memory_items(created_at, updated_at, id, user_id, memory_type, content, scope) VALUES
 ('2026-01-01 12:00:00.000000', '2026-01-01 12:00:00.000000', 1, 1, 'semantic', 'fact', 'global'),
 ('2026-01-01 12:30:00.000000', '2026-01-01 12:30:00.000000', 2, 1, 'episodic', 'event', 'global');
"#;

fn adopted_store() -> (TempDir, LegacyStore) {
    let directory = TempDir::new().expect("tempdir");
    let path = temp_database(directory.path(), "harness.db");
    let seed = format!("{DEFAULT_SEED}\n{ANALYTICS_SEED}");
    create_python_database(&path, &seed);
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

#[test]
fn summary_aggregates_spend_and_tool_success() {
    let (_directory, store) = adopted_store();
    let summary = store.summary("local-user", DAYS).expect("summary");
    assert!((summary.total_spend_usd - 1.75).abs() < 1e-9);
    assert_eq!(summary.total_llm_calls, 3);
    assert_eq!(summary.total_tokens, 465);
    assert_eq!(summary.total_tool_calls, 3);
    assert_eq!(summary.tool_error_count, 1);
    assert!((summary.tool_success_rate - 0.667).abs() < 1e-9);
    assert_eq!(summary.days, DAYS);
}

#[test]
fn spend_buckets_models_and_history_match_python_grouping() {
    let (_directory, store) = adopted_store();

    let daily = store
        .spend_over_time("local-user", DAYS, "day")
        .expect("daily");
    assert_eq!(daily.len(), 2);
    assert_eq!(daily[0].period, "2026-01-01");
    assert!((daily[0].cost_usd - 1.5).abs() < 1e-9);
    assert_eq!(daily[0].total_tokens, 450);
    assert_eq!(daily[0].calls, 2);
    assert_eq!(daily[1].period, "2026-01-02");

    let hourly = store
        .spend_over_time("local-user", DAYS, "hour")
        .expect("hourly");
    assert_eq!(hourly.len(), 3);
    assert_eq!(hourly[0].period, "2026-01-01 10:00");
    assert_eq!(hourly[1].period, "2026-01-01 11:00");

    let by_model = store.spend_by_model("local-user", DAYS).expect("by model");
    assert_eq!(by_model.len(), 2);
    assert_eq!(by_model[0].model, "gpt-4o");
    assert!((by_model[0].cost_usd - 1.5).abs() < 1e-9);
    assert_eq!(by_model[1].model, "claude-3");

    let history = store
        .call_history("local-user", 10, 0, None, None)
        .expect("history");
    assert_eq!(history.len(), 3);
    assert_eq!(history[0].id, 3, "newest first");
    assert_eq!(history[0].provider_name, "anthropic");
    assert_eq!(
        store
            .call_history_total("local-user", None, None)
            .expect("total"),
        3
    );

    let filtered = store
        .call_history("local-user", 10, 0, Some("gpt-4o"), None)
        .expect("by model");
    assert_eq!(filtered.len(), 2);
    let provider = store
        .call_history("local-user", 10, 0, None, Some("anthropic"))
        .expect("by provider");
    assert_eq!(provider.len(), 1);
    assert_eq!(provider[0].id, 3);
}

#[test]
fn tool_and_latency_aggregations_round_trip() {
    let (_directory, store) = adopted_store();
    let tools = store.top_tools("local-user", DAYS, 20).expect("tools");
    assert_eq!(tools.len(), 2);
    assert_eq!(tools[0].name, "read_file");
    assert_eq!(tools[0].calls, 2);
    assert_eq!(tools[0].error_count, 1);
    assert!((tools[0].success_rate - 0.5).abs() < 1e-9);
    assert!((tools[0].avg_duration_ms - 15.0).abs() < 1e-9);
    assert_eq!(tools[1].name, "write_file");

    let latency = store.latency("local-user", DAYS, "day").expect("latency");
    assert_eq!(latency.len(), 1);
    assert_eq!(latency[0].period, "2026-01-01");
    assert!((latency[0].avg_ms - 200.0).abs() < 1e-9);
    assert_eq!(latency[0].min_ms, 100);
    assert_eq!(latency[0].max_ms, 300);
    assert_eq!(latency[0].calls, 2);
}

#[test]
fn memory_activity_groups_by_period_and_type() {
    let (_directory, store) = adopted_store();
    let activity = store
        .memory_activity("local-user", DAYS, "day")
        .expect("activity");
    assert_eq!(activity.len(), 1);
    assert_eq!(activity[0].period, "2026-01-01");
    assert_eq!(activity[0].created, 2);
    let expected: BTreeMap<String, i64> =
        [("semantic".to_string(), 1), ("episodic".to_string(), 1)]
            .into_iter()
            .collect();
    assert_eq!(activity[0].by_type, expected);
}

#[test]
fn analytics_are_actor_scoped_and_validate_buckets() {
    let (_directory, store) = adopted_store();
    store.ensure_actor("other-actor").expect("actor");

    let summary = store.summary("other-actor", DAYS).expect("summary");
    assert_eq!(summary.total_llm_calls, 0);
    assert_eq!(summary.total_spend_usd, 0.0);
    assert_eq!(summary.tool_success_rate, 1.0);
    assert!(
        store
            .spend_over_time("other-actor", DAYS, "day")
            .expect("spend")
            .is_empty()
    );
    assert!(
        store
            .spend_by_model("other-actor", DAYS)
            .expect("model")
            .is_empty()
    );
    assert!(
        store
            .top_tools("other-actor", DAYS, 20)
            .expect("tools")
            .is_empty()
    );
    assert!(
        store
            .latency("other-actor", DAYS, "day")
            .expect("latency")
            .is_empty()
    );
    assert!(
        store
            .call_history("other-actor", 10, 0, None, None)
            .expect("history")
            .is_empty()
    );
    assert_eq!(
        store
            .call_history_total("other-actor", None, None)
            .expect("total"),
        0
    );
    assert!(
        store
            .memory_activity("other-actor", DAYS, "day")
            .expect("memory")
            .is_empty()
    );

    assert!(matches!(
        store.spend_over_time("local-user", DAYS, "week"),
        Err(StoreError::InvalidInput(_))
    ));
    assert!(matches!(
        store.latency("local-user", DAYS, "minute"),
        Err(StoreError::InvalidInput(_))
    ));
    assert!(matches!(
        store.memory_activity("local-user", DAYS, "month"),
        Err(StoreError::InvalidInput(_))
    ));
}
