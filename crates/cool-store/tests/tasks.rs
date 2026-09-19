//! Scheduled-task / task-run store parity tests (CRUD, actor scoping, counters,
//! inbox read state, skipped runs).

mod common;

use common::{DEFAULT_SEED, create_python_database, temp_database};
use cool_store::domains::tasks::{
    NewScheduledTask, NewTaskRun, ScheduledTaskPatch, TRIGGER_CRON, TRIGGER_DATE, TRIGGER_INTERVAL,
};
use cool_store::{LegacyStore, StoreError, parse_python_datetime};
use serde_json::json;
use tempfile::TempDir;

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

fn interval_task(name: &str, seconds: i64) -> NewScheduledTask {
    NewScheduledTask {
        name: name.to_string(),
        prompt: "say hello".to_string(),
        trigger_type: TRIGGER_INTERVAL.to_string(),
        interval_seconds: Some(seconds),
        ..NewScheduledTask::default()
    }
}

#[test]
fn task_crud_computes_and_recomputes_next_run() {
    let (_directory, store) = adopted_store();
    let created = store
        .create_task("local-user", &interval_task("hourly", 3_600))
        .expect("create");
    assert_eq!(created.name, "hourly");
    assert_eq!(created.trigger_type, TRIGGER_INTERVAL);
    assert_eq!(created.timezone, "UTC");
    assert_eq!(created.misfire_policy, "skip");
    assert_eq!(created.approval_policy, "deny_external");
    assert_eq!(created.max_iterations, 10);
    assert!(created.enabled);
    assert_eq!(created.run_count, 0);
    assert_eq!(created.failure_count, 0);
    let first_next = created
        .next_run_at
        .as_deref()
        .and_then(parse_python_datetime)
        .expect("initial next_run_at");
    assert!(first_next > 0);

    let listed = store.list_tasks("local-user", false).expect("list");
    assert_eq!(listed.len(), 1);

    let renamed = store
        .update_task(
            "local-user",
            created.id,
            &ScheduledTaskPatch {
                name: Some("renamed".to_string()),
                interval_seconds: Some(60),
                ..ScheduledTaskPatch::default()
            },
        )
        .expect("update");
    assert_eq!(renamed.name, "renamed");
    assert_eq!(renamed.interval_seconds, Some(60));
    // Changing an interval schedule recomputes next_run_at.
    let recomputed = renamed
        .next_run_at
        .as_deref()
        .and_then(parse_python_datetime)
        .expect("next_run_at");
    assert!(recomputed > 0);
    assert_ne!(recomputed, first_next);

    let disabled = store
        .set_task_enabled("local-user", created.id, false)
        .expect("disable");
    assert!(!disabled.enabled);
    assert_eq!(disabled.next_run_at, None);
    assert_eq!(
        store
            .list_tasks("local-user", true)
            .expect("enabled only")
            .len(),
        0
    );

    let enabled = store
        .set_task_enabled("local-user", created.id, true)
        .expect("enable");
    assert!(enabled.enabled);
    assert!(enabled.next_run_at.is_some());

    store.delete_task("local-user", created.id).expect("delete");
    assert_eq!(
        store.list_tasks("local-user", false).expect("list").len(),
        0
    );
    assert!(matches!(
        store.get_task("local-user", created.id),
        Err(StoreError::NotFound("scheduled task"))
    ));
}

#[test]
fn invalid_schedule_is_rejected_before_any_write() {
    let (_directory, store) = adopted_store();
    let error = store
        .create_task(
            "local-user",
            &NewScheduledTask {
                name: "bad cron".to_string(),
                prompt: "x".to_string(),
                trigger_type: TRIGGER_CRON.to_string(),
                cron_expression: Some("not a cron".to_string()),
                ..NewScheduledTask::default()
            },
        )
        .expect_err("invalid cron");
    assert!(matches!(error, StoreError::InvalidInput(_)));
    assert_eq!(
        store.list_tasks("local-user", false).expect("list").len(),
        0
    );

    // Unknown policies are rejected too (Python `create_task` raises ValueError).
    let bad_policy = store
        .create_task(
            "local-user",
            &NewScheduledTask {
                misfire_policy: "explode".to_string(),
                ..interval_task("bad policy", 60)
            },
        )
        .expect_err("invalid misfire policy");
    assert!(matches!(bad_policy, StoreError::InvalidInput(_)));
    assert_eq!(
        store.list_tasks("local-user", false).expect("list").len(),
        0
    );
}

#[test]
fn tasks_are_actor_scoped() {
    let (_directory, store) = adopted_store();
    let mine = store
        .create_task("local-user", &interval_task("mine", 3_600))
        .expect("create");
    store.ensure_actor("other-actor").expect("register");

    assert_eq!(
        store
            .list_tasks("other-actor", false)
            .expect("other list")
            .len(),
        0
    );
    assert!(matches!(
        store.get_task("other-actor", mine.id),
        Err(StoreError::NotFound("scheduled task"))
    ));
    assert!(matches!(
        store.delete_task("other-actor", mine.id),
        Err(StoreError::NotFound("scheduled task"))
    ));
    // Still visible to its owner.
    assert!(store.get_task("local-user", mine.id).is_ok());
}

#[test]
fn task_runs_round_trip_and_cancel_is_terminal_idempotent() {
    let (_directory, store) = adopted_store();
    let task = store
        .create_task("local-user", &interval_task("runner", 3_600))
        .expect("create");

    let run = store
        .create_task_run(
            "local-user",
            task.id,
            &NewTaskRun {
                trigger_source: "schedule".to_string(),
                prompt: "say hello".to_string(),
                status: "queued".to_string(),
                approval_policy: Some("deny_external".to_string()),
                approval_reason: Some("background run".to_string()),
                skip_reason: None,
            },
        )
        .expect("create run");
    assert_eq!(run.status, "queued");
    assert!(!run.is_read);
    assert!(run.finished_at.is_none());

    let finished = store
        .finish_task_run(
            "local-user",
            run.id,
            "completed",
            Some("hello"),
            None,
            Some(&json!({"total_tokens": 3})),
            Some(120),
            Some(1),
            None,
        )
        .expect("finish");
    assert_eq!(finished.status, "completed");
    assert_eq!(finished.output.as_deref(), Some("hello"));
    assert_eq!(finished.duration_ms, Some(120));
    assert!(finished.finished_at.is_some());

    let listed = store
        .list_task_runs("local-user", task.id, None)
        .expect("list runs");
    assert_eq!(listed.len(), 1);

    let cancelled = store
        .cancel_task_run("local-user", run.id)
        .expect("cancel terminal");
    // Already terminal: returned unchanged.
    assert_eq!(cancelled.status, "completed");
}

#[test]
fn failure_count_auto_disables_after_ceiling_and_success_resets() {
    let (_directory, store) = adopted_store();
    let task = store
        .create_task("local-user", &interval_task("flaky", 3_600))
        .expect("create");

    let after_one = store
        .record_task_outcome(task.id, "failed", false, 3)
        .expect("fail 1");
    assert_eq!(after_one.failure_count, 1);
    assert!(after_one.enabled);

    let after_two = store
        .record_task_outcome(task.id, "failed", false, 3)
        .expect("fail 2");
    assert_eq!(after_two.failure_count, 2);
    assert!(after_two.enabled);

    let disabled = store
        .record_task_outcome(task.id, "failed", false, 3)
        .expect("fail 3");
    assert_eq!(disabled.failure_count, 3);
    assert!(!disabled.enabled, "auto-disabled at the ceiling");
    assert_eq!(disabled.next_run_at, None);
    // `record_task_outcome` mirrors Python `_finalize_run` (no run_count bump;
    // the fire that created the run is what increments it).
    assert_eq!(disabled.run_count, 0);

    // Ceiling of 0 means never auto-disable.
    let mut other = interval_task("tolerant", 3_600);
    other.enabled = true;
    let tolerant = store.create_task("local-user", &other).expect("create");
    let mut last = tolerant.clone();
    for _ in 0..6 {
        last = store
            .record_task_outcome(tolerant.id, "failed", false, 0)
            .expect("fail");
    }
    assert!(last.enabled);
    assert_eq!(last.failure_count, 6);

    // Success resets the streak.
    let reset = store
        .record_task_outcome(tolerant.id, "completed", true, 5)
        .expect("success");
    assert_eq!(reset.failure_count, 0);
    assert!(reset.enabled);
}

#[test]
fn one_shot_date_task_disables_after_terminal_run() {
    let (_directory, store) = adopted_store();
    let run_at = "2030-01-01 09:00:00.000000";
    let task = store
        .create_task(
            "local-user",
            &NewScheduledTask {
                name: "one shot".to_string(),
                prompt: "remind me".to_string(),
                trigger_type: TRIGGER_DATE.to_string(),
                run_at: Some(run_at.to_string()),
                ..NewScheduledTask::default()
            },
        )
        .expect("create");
    assert!(task.enabled);
    assert!(task.next_run_at.is_some());

    let done = store
        .record_task_outcome(task.id, "completed", true, 5)
        .expect("complete");
    assert!(!done.enabled, "one-shot date task disables after its run");
    assert_eq!(done.next_run_at, None);
}

#[test]
fn record_task_fired_advances_next_run_and_counters() {
    let (_directory, store) = adopted_store();
    let task = store
        .create_task("local-user", &interval_task("ticker", 3_600))
        .expect("create");

    store
        .record_task_fired(task.id, Some("2024-01-01 01:00:00.000000"))
        .expect("fired");
    let after = store.get_task("local-user", task.id).expect("reload");
    assert_eq!(after.run_count, 1);
    assert_eq!(after.last_status.as_deref(), Some("running"));
    assert_eq!(
        after.next_run_at.as_deref(),
        Some("2024-01-01 01:00:00.000000")
    );
    assert!(after.last_run_at.is_some());

    // A None next_run_at leaves the value untouched.
    store.record_task_fired(task.id, None).expect("fired again");
    let again = store.get_task("local-user", task.id).expect("reload");
    assert_eq!(again.run_count, 2);
    assert_eq!(
        again.next_run_at.as_deref(),
        Some("2024-01-01 01:00:00.000000")
    );
}

#[test]
fn skipped_runs_are_marked_read_and_bump_last_status() {
    let (_directory, store) = adopted_store();
    let task = store
        .create_task("local-user", &interval_task("quiet", 3_600))
        .expect("create");
    // Pretend the fire time was missed long ago so the recomputed `next_run_at`
    // must move forward.
    store
        .with_connection(|connection| {
            connection.execute(
                "UPDATE scheduled_tasks SET next_run_at = '2020-01-01 00:00:00.000000' WHERE id = ?1",
                [task.id],
            )?;
            Ok(())
        })
        .expect("age the schedule");

    let skipped = store
        .record_skipped_run("local-user", task.id, "quiet hours 23:00 - 07:00")
        .expect("skip");
    assert_eq!(skipped.status, "skipped");
    assert_eq!(skipped.trigger_source, "schedule");
    assert!(skipped.is_read, "skips do not demand inbox attention");
    assert_eq!(
        skipped.skip_reason.as_deref(),
        Some("quiet hours 23:00 - 07:00")
    );
    assert!(skipped.finished_at.is_some());

    let reloaded = store.get_task("local-user", task.id).expect("reload");
    assert_eq!(reloaded.last_status.as_deref(), Some("skipped"));
    // A skip is a fire time: `next_run_at` must move forward (Python
    // `record_skipped_run` recomputes it) or the task stays due forever.
    let previous = "2020-01-01 00:00:00.000000".to_string();
    let next = reloaded.next_run_at.clone().expect("next run advances");
    assert_ne!(next, previous);
    assert!(parse_python_datetime(&next).unwrap() > parse_python_datetime(&previous).unwrap());
}

#[test]
fn inbox_lists_newest_first_and_read_markers_are_actor_scoped() {
    let (_directory, store) = adopted_store();
    let task = store
        .create_task("local-user", &interval_task("inbox", 3_600))
        .expect("create");

    for prompt in ["first", "second", "third"] {
        let run = store
            .create_task_run(
                "local-user",
                task.id,
                &NewTaskRun {
                    prompt: prompt.to_string(),
                    ..NewTaskRun::default()
                },
            )
            .expect("create run");
        store
            .finish_task_run(
                "local-user",
                run.id,
                "completed",
                Some(prompt),
                None,
                None,
                None,
                None,
                None,
            )
            .expect("finish");
    }

    let inbox = store
        .list_task_inbox("local-user", false, None)
        .expect("inbox");
    assert_eq!(inbox.len(), 3);
    assert_eq!(inbox[0].output.as_deref(), Some("third"));

    let unread = store
        .list_task_inbox("local-user", true, None)
        .expect("unread");
    assert_eq!(unread.len(), 3);

    let marked = store
        .mark_task_run_read("local-user", inbox[0].id, true)
        .expect("mark read");
    assert!(marked.is_read);
    let unread_after = store
        .list_task_inbox("local-user", true, None)
        .expect("unread after");
    assert_eq!(unread_after.len(), 2);

    // Another actor cannot read or mark the run.
    store.ensure_actor("other-actor").expect("register");
    assert!(matches!(
        store.get_task_run("other-actor", inbox[0].id),
        Err(StoreError::NotFound("scheduled task"))
    ));
    assert!(matches!(
        store.mark_task_run_read("other-actor", inbox[0].id, true),
        Err(StoreError::NotFound("scheduled task"))
    ));
    assert_eq!(
        store
            .list_task_inbox("other-actor", false, None)
            .expect("other inbox")
            .len(),
        0
    );
}

#[test]
fn deleting_a_task_removes_its_run_history() {
    let (_directory, store) = adopted_store();
    let task = store
        .create_task("local-user", &interval_task("ephemeral", 3_600))
        .expect("create");
    store
        .create_task_run("local-user", task.id, &NewTaskRun::default())
        .expect("run");

    store
        .delete_task("local-user", task.id)
        .expect("delete task");
    assert!(matches!(
        store.list_task_runs("local-user", task.id, None),
        Err(StoreError::NotFound("scheduled task"))
    ));
    assert_eq!(
        store
            .list_task_inbox("local-user", false, None)
            .expect("inbox")
            .len(),
        0
    );
}
