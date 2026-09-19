//! Store-level tests for the M10 protocol-support additions: idempotency
//! reservations, budget status windows, profile seeding, wiki tag filters,
//! draft plan edits, inbox counters, recent working directories, memory
//! explanations and cron parsing.

use cool_store::domains::budgets::{BudgetUpdate, NewSpendEntry};
use cool_store::domains::conversations::{ConversationPatch, NewConversation};
use cool_store::domains::memory::NewMemoryItem;
use cool_store::domains::tasks::{NewScheduledTask, NewTaskRun};
use cool_store::domains::wiki::{NewWikiArticle, WikiFilter};
use cool_store::{LegacyStore, StoreError};
use serde_json::json;

fn store() -> LegacyStore {
    let store = LegacyStore::in_memory().expect("in-memory store");
    store.ensure_actor("local-user").expect("actor");
    store
}

fn conversation(store: &LegacyStore, title: &str, working_directory: Option<&str>) -> i64 {
    store
        .create_conversation(
            "local-user",
            &NewConversation {
                title: Some(title.to_owned()),
                working_directory: working_directory.map(str::to_owned),
                ..NewConversation::default()
            },
        )
        .expect("conversation")
        .id
}

fn scheduled_task(store: &LegacyStore, name: &str) -> i64 {
    store
        .create_task(
            "local-user",
            &NewScheduledTask {
                name: name.to_owned(),
                description: None,
                trigger_type: "interval".to_owned(),
                cron_expression: None,
                interval_seconds: Some(3_600),
                run_at: None,
                timezone: "UTC".to_owned(),
                quiet_hours_start: None,
                quiet_hours_end: None,
                misfire_policy: "run".to_owned(),
                prompt: "tick".to_owned(),
                workflow_type: None,
                profile_id: None,
                model: None,
                tools_whitelist: None,
                capability_policy: None,
                working_directory: None,
                approval_policy: "deny_external".to_owned(),
                delivery_channels: None,
                delivery_config: None,
                max_iterations: 3,
                max_cost_per_run: None,
                timeout_s: None,
                enabled: true,
            },
        )
        .expect("task")
        .id
}

fn memory(store: &LegacyStore, content: &str) -> i64 {
    store
        .create_memory_item(
            "local-user",
            &NewMemoryItem {
                scope: "global".to_owned(),
                agent_id: None,
                conversation_id: None,
                memory_type: "semantic".to_owned(),
                content: content.to_owned(),
                structured: None,
                tags: None,
                importance: 0.5,
                confidence: 0.7,
                source: "user_explicit".to_owned(),
                status: None,
                confirmed: false,
                supersedes_id: None,
                ttl_days: None,
                valid_from: None,
                valid_to: None,
                pinned: false,
            },
        )
        .expect("memory")
        .id
}

#[test]
fn idempotency_replays_and_fails_closed() {
    let store = store();
    let fingerprint = "{\"content\":\"once\"}";
    let first = store
        .run_idempotent("local-user", "memory.create", "key-1", fingerprint, || {
            Ok(memory(&store, "once"))
        })
        .expect("first run");
    assert!(first.created);

    let replay = store
        .run_idempotent::<i64, _>("local-user", "memory.create", "key-1", fingerprint, || {
            panic!("replay must not execute the action")
        })
        .expect("replay");
    assert!(!replay.created);
    assert_eq!(replay.value, first.value);

    let conflict = store
        .run_idempotent::<i64, _>("local-user", "memory.create", "key-1", "other", || Ok(2))
        .expect_err("fingerprint mismatch");
    assert!(matches!(conflict, StoreError::Conflict(_)));

    store
        .with_connection(|connection| {
            connection.execute(
                "INSERT INTO rust_idempotency(actor_id, method, idempotency_key, fingerprint,
                   status, result_json, created_at, updated_at)
                 VALUES ('local-user', 'memory.delete', 'pending-key', 'fp', 'pending', NULL,
                         '2026-01-01 00:00:00.000000', '2026-01-01 00:00:00.000000')",
                [],
            )?;
            Ok(())
        })
        .expect("pending reservation");
    let pending = store
        .run_idempotent::<i64, _>("local-user", "memory.delete", "pending-key", "fp", || Ok(1))
        .expect_err("pending reservation fails closed");
    assert!(matches!(pending, StoreError::Conflict(_)));
}

#[tokio::test]
async fn async_idempotency_matches_the_sync_contract() {
    let store = store();
    let first = store
        .run_idempotent_async(
            "local-user",
            "workspace.git_checkout",
            "key-1",
            "fp",
            || async { Ok(json!({"branch": "feature"})) },
        )
        .await
        .expect("first run");
    assert!(first.created);
    let replay = store
        .run_idempotent_async::<serde_json::Value, _, _>(
            "local-user",
            "workspace.git_checkout",
            "key-1",
            "fp",
            || async { panic!("replay must not execute the action") },
        )
        .await
        .expect("replay");
    assert!(!replay.created);
    assert_eq!(replay.value, json!({"branch": "feature"}));
}

#[test]
fn budget_status_uses_calendar_windows_and_override() {
    let store = store();
    store
        .upsert_budget(
            "local-user",
            &BudgetUpdate {
                daily_limit_usd: Some(1.0),
                alert_threshold_pct: Some(50.0),
                block_on_exceed: Some(true),
                ..BudgetUpdate::default()
            },
        )
        .expect("budget");
    let now = cool_store::parse_python_datetime("2026-06-15 12:00:00").unwrap();
    store
        .log_spend(
            "local-user",
            &NewSpendEntry {
                provider_name: "openai".to_owned(),
                model: "gpt-4o".to_owned(),
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
                cost_usd: 0.8,
                ts: Some("2026-06-15 09:00:00".to_owned()),
                ..NewSpendEntry::default()
            },
        )
        .expect("spend");

    let status = store.budget_status("local-user", now).expect("status");
    assert_eq!(status.status, "alert");
    assert_eq!(status.daily.spend_usd, 0.8);
    assert_eq!(status.weekly.spend_usd, 0.8);
    assert_eq!(status.monthly.spend_usd, 0.8);

    store
        .log_spend(
            "local-user",
            &NewSpendEntry {
                provider_name: "openai".to_owned(),
                model: "gpt-4o".to_owned(),
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
                cost_usd: 0.5,
                ts: Some("2026-06-14 09:00:00".to_owned()),
                ..NewSpendEntry::default()
            },
        )
        .expect("spend");
    store
        .log_spend(
            "local-user",
            &NewSpendEntry {
                provider_name: "openai".to_owned(),
                model: "gpt-4o".to_owned(),
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
                cost_usd: 0.3,
                ts: Some("2026-06-15 10:00:00".to_owned()),
                ..NewSpendEntry::default()
            },
        )
        .expect("spend");
    let status = store.budget_status("local-user", now).expect("status");
    assert_eq!(status.status, "blocked");
    assert_eq!(status.daily.spend_usd, 1.1);
    // 2026-06-15 is a Monday, so Sunday's spend is outside the weekly window.
    assert_eq!(status.weekly.spend_usd, 1.1);

    store
        .set_budget_override("local-user", Some("2026-06-15 13:00:00"))
        .expect("override");
    let status = store.budget_status("local-user", now).expect("status");
    assert!(status.overridden);
    assert_eq!(status.status, "alert");

    store
        .set_budget_override("local-user", None)
        .expect("clear");
    let status = store.budget_status("local-user", now).expect("status");
    assert_eq!(status.status, "blocked");
}

#[test]
fn profile_seeding_is_idempotent_and_preserves_edits() {
    let store = store();
    assert_eq!(store.seed_builtin_profiles().expect("seed"), 5);
    assert_eq!(store.seed_builtin_profiles().expect("reseed"), 0);
    let profiles = store.list_profiles(true).expect("profiles");
    assert_eq!(profiles.len(), 5);
    assert!(profiles.iter().all(|profile| profile.is_builtin));

    let coder = profiles
        .iter()
        .find(|profile| profile.slug == "coder")
        .expect("coder");
    store
        .update_profile(
            coder.id,
            &cool_store::domains::profiles::AgentProfilePatch {
                name: Some("Coder Custom".to_owned()),
                ..Default::default()
            },
        )
        .expect("edit");
    assert_eq!(store.seed_builtin_profiles().expect("reseed"), 0);
    assert_eq!(
        store.get_profile(coder.id).expect("get").name,
        "Coder Custom"
    );
}

#[test]
fn wiki_tag_filter_and_plan_draft_edits_are_store_backed() {
    let store = store();
    store
        .create_article(
            "local-user",
            &NewWikiArticle {
                title: "Rust".to_owned(),
                content: "systems".to_owned(),
                category: "dev".to_owned(),
                tags: Some(json!(["rust", "sqlite"])),
                source: "manual".to_owned(),
                source_memory_id: None,
                project_key: None,
                metadata: None,
            },
        )
        .expect("article");
    let filtered = store
        .list_articles(
            "local-user",
            &WikiFilter {
                tag: Some("rust".to_owned()),
                ..WikiFilter::default()
            },
        )
        .expect("filter");
    assert_eq!(filtered.len(), 1);
    let missing = store
        .list_articles(
            "local-user",
            &WikiFilter {
                tag: Some("python".to_owned()),
                ..WikiFilter::default()
            },
        )
        .expect("filter");
    assert!(missing.is_empty());

    let conversation_id = conversation(&store, "plans", None);
    let plan = store
        .create_plan(
            "local-user",
            conversation_id,
            None,
            Some("Draft"),
            &json!([{"position": 0, "title": "One"}]),
        )
        .expect("plan");
    let updated = store
        .update_plan_draft(
            "local-user",
            conversation_id,
            plan.id,
            Some("Edited"),
            Some(&json!([
                {"position": 0, "title": "One"},
                {"position": 1, "title": "Two"}
            ])),
        )
        .expect("update");
    assert_eq!(updated.title.as_deref(), Some("Edited"));
    assert_eq!(store.list_plan_steps(plan.id).expect("steps").len(), 2);
    store
        .set_plan_status("local-user", conversation_id, plan.id, "approved")
        .expect("approve");
    let rejected = store
        .update_plan_draft("local-user", conversation_id, plan.id, Some("Nope"), None)
        .expect_err("approved plans are immutable");
    assert!(matches!(rejected, StoreError::InvalidInput(_)));
}

#[test]
fn task_inbox_counts_and_recent_directories() {
    let store = store();
    let task_id = scheduled_task(&store, "digest");
    let first = store
        .create_task_run(
            "local-user",
            task_id,
            &NewTaskRun {
                trigger_source: "manual".to_owned(),
                prompt: "run".to_owned(),
                status: "queued".to_owned(),
                approval_policy: None,
                approval_reason: None,
                skip_reason: None,
            },
        )
        .expect("run");
    store
        .create_task_run(
            "local-user",
            task_id,
            &NewTaskRun {
                trigger_source: "manual".to_owned(),
                prompt: "run".to_owned(),
                status: "queued".to_owned(),
                approval_policy: None,
                approval_reason: None,
                skip_reason: None,
            },
        )
        .expect("run");
    assert_eq!(
        store.count_unread_task_runs("local-user").expect("count"),
        2
    );
    store
        .mark_task_run_read("local-user", first.id, true)
        .expect("read");
    assert_eq!(
        store.count_unread_task_runs("local-user").expect("count"),
        1
    );

    let first_conversation = conversation(&store, "one", Some("C:/one"));
    conversation(&store, "two", Some("C:/two"));
    conversation(&store, "three", Some("C:/one"));
    store
        .update_conversation(
            "local-user",
            first_conversation,
            &ConversationPatch {
                title: Some("one-updated".to_owned()),
                ..Default::default()
            },
        )
        .expect("update");
    let recent = store
        .recent_working_directories("local-user", 10)
        .expect("recent");
    assert_eq!(recent, vec!["C:/one".to_owned(), "C:/two".to_owned()]);
}

#[test]
fn memory_explanation_and_cron_parsing() {
    let store = store();
    let memory_id = memory(&store, "explain me");
    let explanation = store
        .explain_memory("local-user", memory_id)
        .expect("explain");
    assert_eq!(explanation.memory_id, memory_id);
    assert!(explanation.score.importance > 0.0);
    assert!(explanation.score.total >= explanation.score.importance);

    let runs = cool_store::scheduler::cron_next_runs("0 8 * * *", 0, 3).expect("cron");
    assert_eq!(runs.len(), 3);
    assert!(runs[0] <= runs[1] && runs[1] <= runs[2]);
    let invalid = cool_store::scheduler::cron_next_runs("not a schedule", 0, 1);
    assert!(invalid.is_err());
}
