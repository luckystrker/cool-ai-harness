//! Budgets / spend-log store parity tests.

mod common;

use common::{DEFAULT_SEED, create_python_database, temp_database};
use cool_store::domains::budgets::{BudgetUpdate, NewSpendEntry};
use cool_store::{LegacyStore, StoreError};
use tempfile::TempDir;

/// Unix timestamp for 2026-01-01 00:00:00 UTC.
const JAN_1: i64 = 1_767_225_600;
/// Unix timestamp for 2026-01-02 00:00:00 UTC.
const JAN_2: i64 = 1_767_312_000;

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

fn spend(ts: &str, cost_usd: f64) -> NewSpendEntry {
    NewSpendEntry {
        provider_name: "openai".to_string(),
        model: "gpt-4o".to_string(),
        prompt_tokens: 10,
        completion_tokens: 5,
        total_tokens: 15,
        cost_usd,
        ts: Some(ts.to_string()),
        ..NewSpendEntry::default()
    }
}

#[test]
fn budget_row_is_lazy_created_and_patched_field_by_field() {
    let (_directory, store) = adopted_store();
    assert!(store.get_budget("local-user").expect("read").is_none());

    let created = store
        .upsert_budget(
            "local-user",
            &BudgetUpdate {
                daily_limit_usd: Some(5.0),
                alert_threshold_pct: Some(70.0),
                block_on_exceed: Some(false),
                ..BudgetUpdate::default()
            },
        )
        .expect("upsert");
    assert_eq!(created.daily_limit_usd, Some(5.0));
    assert_eq!(created.alert_threshold_pct, 70.0);
    assert!(!created.block_on_exceed);
    assert_eq!(created.user_id, 1);

    let patched = store
        .upsert_budget(
            "local-user",
            &BudgetUpdate {
                weekly_limit_usd: Some(40.0),
                ..BudgetUpdate::default()
            },
        )
        .expect("patch");
    assert_eq!(patched.daily_limit_usd, Some(5.0), "unchanged");
    assert_eq!(patched.weekly_limit_usd, Some(40.0));

    let overridden = store
        .set_budget_override("local-user", Some("2026-12-31 00:00:00.000000"))
        .expect("override");
    assert_eq!(
        overridden.override_until.as_deref(),
        Some("2026-12-31 00:00:00.000000")
    );
    assert!(
        store
            .set_budget_override("local-user", None)
            .expect("clear")
            .override_until
            .is_none()
    );

    store
        .touch_budget_alert("local-user", "2026-01-01 00:00:00.000000")
        .expect("alert");
    assert_eq!(
        store
            .get_budget("local-user")
            .expect("read")
            .expect("row")
            .last_alert_at
            .as_deref(),
        Some("2026-01-01 00:00:00.000000")
    );
}

#[test]
fn invalid_budget_updates_are_rejected() {
    let (_directory, store) = adopted_store();
    assert!(matches!(
        store.upsert_budget(
            "local-user",
            &BudgetUpdate {
                daily_limit_usd: Some(-1.0),
                ..BudgetUpdate::default()
            }
        ),
        Err(StoreError::InvalidInput(_))
    ));
    assert!(matches!(
        store.upsert_budget(
            "local-user",
            &BudgetUpdate {
                alert_threshold_pct: Some(150.0),
                ..BudgetUpdate::default()
            }
        ),
        Err(StoreError::InvalidInput(_))
    ));
    assert!(
        store.get_budget("local-user").expect("read").is_none(),
        "failed validation must not create a row"
    );
}

#[test]
fn spend_log_orders_newest_first_and_summarizes() {
    let (_directory, store) = adopted_store();
    let first = store
        .log_spend("local-user", &spend("2026-01-01 00:00:00.000000", 0.25))
        .expect("log first");
    let second = store
        .log_spend("local-user", &spend("2026-01-02 00:00:00.000000", 0.25))
        .expect("log second");
    assert!(second > first);

    let all = store.list_spend("local-user", None, None).expect("list");
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].id, second, "newest first");
    assert_eq!(all[0].ts, "2026-01-02 00:00:00.000000");

    let windowed = store
        .list_spend("local-user", Some(JAN_2), None)
        .expect("windowed");
    assert_eq!(windowed.len(), 1);
    assert_eq!(windowed[0].id, second);

    let summary = store.spend_summary("local-user", None).expect("summary");
    assert_eq!(summary.calls, 2);
    assert_eq!(summary.prompt_tokens, 20);
    assert_eq!(summary.completion_tokens, 10);
    assert_eq!(summary.total_tokens, 30);
    assert!((summary.cost_usd - 0.5).abs() < 1e-9);

    let jan2_summary = store
        .spend_summary("local-user", Some(JAN_2))
        .expect("summary");
    assert_eq!(jan2_summary.calls, 1);
    assert!((jan2_summary.cost_usd - 0.25).abs() < 1e-9);
    let jan1_summary = store
        .spend_summary("local-user", Some(JAN_1))
        .expect("summary");
    assert_eq!(jan1_summary.calls, 2);
}

#[test]
fn budgets_and_spend_are_actor_scoped() {
    let (_directory, store) = adopted_store();
    store.ensure_actor("other-actor").expect("actor");
    assert!(store.get_budget("other-actor").expect("read").is_none());

    let other = store
        .upsert_budget(
            "other-actor",
            &BudgetUpdate {
                daily_limit_usd: Some(1.0),
                ..BudgetUpdate::default()
            },
        )
        .expect("upsert other");
    assert_eq!(other.user_id, 2);
    assert_eq!(other.daily_limit_usd, Some(1.0));
    assert!(
        store.get_budget("local-user").expect("local").is_none(),
        "budgets are per actor"
    );
    assert!(
        store
            .list_spend("other-actor", None, None)
            .expect("spend")
            .is_empty()
    );

    store
        .log_spend("local-user", &spend("2026-01-01 00:00:00.000000", 1.0))
        .expect("log");
    assert!(
        store
            .list_spend("other-actor", None, None)
            .expect("spend")
            .is_empty()
    );
    assert_eq!(
        store
            .spend_summary("local-user", None)
            .expect("summary")
            .calls,
        1
    );

    let mut unknown = spend("2026-01-01 00:00:00.000000", 1.0);
    unknown.conversation_id = Some(9_999);
    assert!(matches!(
        store.log_spend("local-user", &unknown),
        Err(StoreError::NotFound("conversation"))
    ));
}
