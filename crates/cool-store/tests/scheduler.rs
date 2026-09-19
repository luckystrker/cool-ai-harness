//! Deterministic scheduler engine parity tests (restart, catch-up, misfire,
//! overlap, quiet hours, timezone scope).

mod common;

use common::{DEFAULT_SEED, create_python_database, temp_database};
use cool_store::domains::tasks::{NewScheduledTask, ScheduledTask, TRIGGER_CRON, TRIGGER_INTERVAL};
use cool_store::scheduler::{
    Decision, ScheduleError, Scheduler, SchedulerConfig, Trigger, next_run, quiet_hours,
};
use cool_store::{LegacyStore, python_datetime};
use tempfile::TempDir;

/// 2024-01-01 00:00:00 UTC (a Monday).
const MONDAY: i64 = 1_704_067_200;

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

/// Build an in-memory task row with sensible defaults for engine tests.
fn task(id: i64, trigger_type: &str) -> ScheduledTask {
    ScheduledTask {
        id,
        user_id: 1,
        name: format!("task-{id}"),
        description: None,
        trigger_type: trigger_type.to_string(),
        cron_expression: None,
        interval_seconds: None,
        run_at: None,
        timezone: "UTC".to_string(),
        quiet_hours_start: None,
        quiet_hours_end: None,
        misfire_policy: "skip".to_string(),
        prompt: "do the thing".to_string(),
        workflow_type: None,
        profile_id: None,
        model: None,
        tools_whitelist: None,
        capability_policy: None,
        working_directory: None,
        approval_policy: "deny_external".to_string(),
        delivery_channels: None,
        delivery_config: None,
        last_delivery_hash: None,
        max_iterations: 10,
        max_cost_per_run: None,
        timeout_s: None,
        enabled: true,
        next_run_at: None,
        last_run_at: None,
        last_status: None,
        run_count: 0,
        failure_count: 0,
        created_at: python_datetime(MONDAY, 0),
        updated_at: python_datetime(MONDAY, 0),
    }
}

#[test]
fn cron_next_run_matches_known_expressions() {
    let mut daily = task(1, TRIGGER_CRON);
    daily.cron_expression = Some("0 0 * * *".to_string());
    assert_eq!(next_run(&daily, MONDAY).unwrap(), Some(MONDAY + 86_400));

    let mut quarter = task(2, TRIGGER_CRON);
    quarter.cron_expression = Some("*/15 * * * *".to_string());
    assert_eq!(next_run(&quarter, MONDAY).unwrap(), Some(MONDAY + 900));

    // Monday 09:00, numeric day-of-week.
    let mut monday_nine = task(3, TRIGGER_CRON);
    monday_nine.cron_expression = Some("0 9 * * 1".to_string());
    assert_eq!(
        next_run(&monday_nine, MONDAY).unwrap(),
        Some(MONDAY + 9 * 3_600)
    );

    // Day-of-month with a range/step hour list.
    let mut month_first = task(4, TRIGGER_CRON);
    month_first.cron_expression = Some("30 8 1 * *".to_string());
    assert_eq!(
        next_run(&month_first, MONDAY).unwrap(),
        Some(MONDAY + 8 * 3_600 + 1_800)
    );
    let mut hour_step = task(5, TRIGGER_CRON);
    hour_step.cron_expression = Some("0 9-17/2 * * *".to_string());
    assert_eq!(
        next_run(&hour_step, MONDAY).unwrap(),
        Some(MONDAY + 9 * 3_600)
    );

    // 3-letter month and day names.
    let mut named_month = task(6, TRIGGER_CRON);
    named_month.cron_expression = Some("0 0 1 jan *".to_string());
    assert_eq!(next_run(&named_month, MONDAY).unwrap(), Some(1_735_689_600));
    let mut named_day = task(7, TRIGGER_CRON);
    named_day.cron_expression = Some("0 12 * * mon".to_string());
    assert_eq!(
        next_run(&named_day, MONDAY).unwrap(),
        Some(MONDAY + 12 * 3_600)
    );

    // Weekday range rolls to the next day.
    let mut weekdays = task(8, TRIGGER_CRON);
    weekdays.cron_expression = Some("0 0 * * 1-5".to_string());
    assert_eq!(next_run(&weekdays, MONDAY).unwrap(), Some(MONDAY + 86_400));

    // Month rollover.
    let mut month_end = task(9, TRIGGER_CRON);
    month_end.cron_expression = Some("0 0 31 * *".to_string());
    assert_eq!(
        next_run(&month_end, MONDAY).unwrap(),
        Some(MONDAY + 30 * 86_400)
    );

    // Explicit Trigger::from_task normalization.
    assert_eq!(
        Trigger::from_task(&quarter).unwrap(),
        Trigger::Cron("*/15 * * * *".to_string())
    );
}

#[test]
fn restart_and_catch_up_runs_once_and_advances_next_run() {
    let (_directory, store) = adopted_store();
    let now = MONDAY + 100_000;
    let created = store
        .create_task(
            "local-user",
            &NewScheduledTask {
                name: "heartbeat".to_string(),
                prompt: "ping".to_string(),
                trigger_type: TRIGGER_INTERVAL.to_string(),
                interval_seconds: Some(3_600),
                // Catch-up after downtime runs once regardless of how late.
                misfire_policy: "run".to_string(),
                ..NewScheduledTask::default()
            },
        )
        .expect("create");

    // Simulate three missed interval fires while the process was down.
    let overdue = now - 3 * 3_600;
    store
        .with_connection(|connection| {
            connection.execute(
                "UPDATE scheduled_tasks SET next_run_at = ?1 WHERE id = ?2",
                rusqlite::params![python_datetime(overdue, 0), created.id],
            )?;
            Ok(())
        })
        .expect("seed overdue");

    let due = store.list_due_tasks(now).expect("due");
    assert_eq!(due.len(), 1);

    let mut scheduler = Scheduler::new(SchedulerConfig::default());
    let decisions = scheduler.plan(&due, now).expect("plan");
    assert_eq!(
        decisions,
        vec![Decision::Execute {
            task_id: created.id,
            scheduled_for: overdue,
        }]
    );

    scheduler.complete(created.id);
    let updated = store
        .record_task_outcome(created.id, "completed", true, 5)
        .expect("outcome");
    let next = updated
        .next_run_at
        .as_deref()
        .and_then(cool_store::parse_python_datetime)
        .expect("next_run_at");
    assert!(next > now, "next run {next} must be in the future of {now}");
}

#[test]
fn misfire_skip_respects_grace_and_policy() {
    let config = SchedulerConfig {
        misfire_grace_seconds: 300,
        max_consecutive_failures: 5,
    };
    let now = MONDAY + 100_000;
    let overdue = now - 600;

    let mut skipping = task(1, TRIGGER_INTERVAL);
    skipping.interval_seconds = Some(3_600);
    skipping.misfire_policy = "skip".to_string();
    skipping.next_run_at = Some(python_datetime(overdue, 0));

    let mut scheduler = Scheduler::new(config.clone());
    let decisions = scheduler.plan(&[skipping], now).expect("plan");
    match &decisions[..] {
        [Decision::Skip { task_id, reason }] => {
            assert_eq!(*task_id, 1);
            assert!(reason.contains("missed"), "unexpected reason {reason:?}");
        }
        other => panic!("expected skip, got {other:?}"),
    }
    assert!(scheduler.running().is_empty());

    let mut running = task(2, TRIGGER_INTERVAL);
    running.interval_seconds = Some(3_600);
    running.misfire_policy = "run".to_string();
    running.next_run_at = Some(python_datetime(overdue, 0));

    let mut scheduler = Scheduler::new(config);
    let decisions = scheduler.plan(&[running], now).expect("plan");
    assert_eq!(
        decisions,
        vec![Decision::Execute {
            task_id: 2,
            scheduled_for: overdue,
        }]
    );

    // Inside the grace window even a skip policy still executes.
    let mut late_but_ok = task(3, TRIGGER_INTERVAL);
    late_but_ok.interval_seconds = Some(3_600);
    late_but_ok.next_run_at = Some(python_datetime(now - 60, 0));
    let mut scheduler = Scheduler::new(SchedulerConfig {
        misfire_grace_seconds: 300,
        max_consecutive_failures: 5,
    });
    assert!(matches!(
        scheduler
            .plan(&[late_but_ok], now)
            .expect("plan")
            .as_slice(),
        [Decision::Execute { .. }]
    ));
}

#[test]
fn overlap_is_suppressed_while_a_task_is_still_running() {
    let now = MONDAY + 100_000;
    let mut recurring = task(7, TRIGGER_INTERVAL);
    recurring.interval_seconds = Some(60);
    recurring.misfire_policy = "run".to_string();
    recurring.next_run_at = Some(python_datetime(now - 1, 0));

    let mut scheduler = Scheduler::new(SchedulerConfig::default());
    let first = scheduler.plan(&[recurring.clone()], now).expect("first");
    assert!(matches!(first.as_slice(), [Decision::Execute { .. }]));

    // Same task, still in-flight: suppressed as overlap even with policy "run".
    let second = scheduler
        .plan(&[recurring.clone()], now + 5)
        .expect("second");
    match &second[..] {
        [Decision::Skip { task_id, reason }] => {
            assert_eq!(*task_id, 7);
            assert_eq!(reason, "overlap");
        }
        other => panic!("expected overlap skip, got {other:?}"),
    }

    // Completing clears the guard.
    scheduler.complete(7);
    assert!(scheduler.running().is_empty());
    let third = scheduler.plan(&[recurring], now + 10).expect("third");
    assert!(matches!(third.as_slice(), [Decision::Execute { .. }]));
}

#[test]
fn quiet_hours_wrap_midnight_matches_python() {
    let mut wrapping = task(1, TRIGGER_CRON);
    wrapping.quiet_hours_start = Some("23:00".to_string());
    wrapping.quiet_hours_end = Some("07:00".to_string());

    // 2024-01-01 23:30 UTC -> inside.
    assert!(quiet_hours(&wrapping, 1_704_151_800).unwrap());
    // 2024-01-02 06:30 UTC -> inside.
    assert!(quiet_hours(&wrapping, 1_704_177_000).unwrap());
    // 2024-01-02 07:00 UTC -> outside (half-open window).
    assert!(!quiet_hours(&wrapping, 1_704_178_800).unwrap());
    // 2024-01-01 12:00 UTC -> outside.
    assert!(!quiet_hours(&wrapping, 1_704_110_400).unwrap());
    // 2024-01-01 22:59 UTC -> outside.
    assert!(!quiet_hours(&wrapping, 1_704_149_940).unwrap());

    let mut daytime = task(2, TRIGGER_CRON);
    daytime.quiet_hours_start = Some("09:00".to_string());
    daytime.quiet_hours_end = Some("17:00".to_string());
    // 12:00 inside; 09:00 inclusive start inside; 08:00 and 17:00 outside.
    assert!(quiet_hours(&daytime, MONDAY + 12 * 3_600).unwrap());
    assert!(quiet_hours(&daytime, MONDAY + 9 * 3_600).unwrap());
    assert!(!quiet_hours(&daytime, MONDAY + 8 * 3_600).unwrap());
    assert!(!quiet_hours(&daytime, MONDAY + 17 * 3_600).unwrap());

    // Missing or equal bounds mean "no quiet hours".
    let mut none = task(3, TRIGGER_CRON);
    none.quiet_hours_start = Some("00:00".to_string());
    none.quiet_hours_end = Some("00:00".to_string());
    assert!(!quiet_hours(&none, 1_704_110_400).unwrap());
}

#[test]
fn plan_skips_fires_inside_quiet_hours_and_does_not_mark_them_running() {
    // The task is due at 09:00 but its quiet window covers 09:00-17:00, so the
    // scheduled fire is gated exactly like Python's `gate_run`.
    let due = MONDAY + 9 * 3_600;
    let mut task = task(9, TRIGGER_CRON);
    task.cron_expression = Some("0 9 * * *".to_string());
    task.quiet_hours_start = Some("09:00".to_string());
    task.quiet_hours_end = Some("17:00".to_string());
    task.next_run_at = Some(python_datetime(due, 0));

    let mut scheduler = Scheduler::new(SchedulerConfig::default());
    let decisions = scheduler.plan(&[task.clone()], due + 5).expect("plan");
    match &decisions[..] {
        [Decision::Skip { task_id, reason }] => {
            assert_eq!(*task_id, 9);
            assert!(
                reason.contains("quiet hours 09:00 - 17:00"),
                "got {reason:?}"
            );
        }
        other => panic!("expected quiet-hours skip, got {other:?}"),
    }
    assert!(scheduler.running().is_empty());

    // Outside the window the same fire executes.
    task.next_run_at = Some(python_datetime(MONDAY + 18 * 3_600, 0));
    let later = scheduler
        .plan(&[task], MONDAY + 18 * 3_600 + 1)
        .expect("plan later");
    assert!(matches!(later.as_slice(), [Decision::Execute { .. }]));
}

#[test]
fn fixed_offset_grammar_rejects_python_incompatible_forms() {
    // Python's zoneinfo path rejects bare offsets like "UTC+5"; the Rust store
    // must fail closed instead of silently accepting a different schedule.
    for timezone in ["UTC+5", "GMT+5", "+5", "+5:30"] {
        let mut task = task(4, TRIGGER_CRON);
        task.timezone = timezone.to_string();
        task.cron_expression = Some("0 0 * * *".to_string());
        assert_eq!(
            next_run(&task, MONDAY).expect_err("must fail closed"),
            ScheduleError::UnsupportedTimezone(timezone.to_string()),
            "timezone {timezone:?} must be rejected"
        );
    }
    // Colon and compact four-digit offsets remain supported.
    for timezone in ["+05:00", "-0500", "UTC+05:00"] {
        let mut task = task(5, TRIGGER_CRON);
        task.timezone = timezone.to_string();
        task.cron_expression = Some("0 0 * * *".to_string());
        assert!(
            next_run(&task, MONDAY).unwrap().is_some(),
            "timezone {timezone:?}"
        );
    }
}

#[test]
fn unsupported_named_timezone_fails_closed() {
    let mut berlin = task(1, TRIGGER_CRON);
    berlin.timezone = "Europe/Berlin".to_string();
    berlin.cron_expression = Some("0 0 * * *".to_string());
    let error = next_run(&berlin, MONDAY).expect_err("IANA tz must fail closed");
    assert_eq!(
        error,
        ScheduleError::UnsupportedTimezone("Europe/Berlin".to_string())
    );
    // Quiet hours only consult the timezone when a window exists (Python
    // short-circuits missing bounds first).
    berlin.quiet_hours_start = Some("23:00".to_string());
    berlin.quiet_hours_end = Some("07:00".to_string());
    assert_eq!(
        quiet_hours(&berlin, MONDAY).expect_err("quiet hours must fail too"),
        ScheduleError::UnsupportedTimezone("Europe/Berlin".to_string())
    );

    // UTC (case-insensitive) and fixed offsets are accepted.
    let mut utc = task(2, TRIGGER_CRON);
    utc.timezone = "utc".to_string();
    utc.cron_expression = Some("0 0 * * *".to_string());
    assert_eq!(next_run(&utc, MONDAY).unwrap(), Some(MONDAY + 86_400));

    let mut plus_two = task(3, TRIGGER_CRON);
    plus_two.timezone = "+02:00".to_string();
    plus_two.cron_expression = Some("0 0 * * *".to_string());
    // Local midnight in +02:00 is 22:00 UTC on the same civil day.
    assert_eq!(
        next_run(&plus_two, MONDAY).unwrap(),
        Some(MONDAY + 22 * 3_600)
    );
}
