//! Legacy `scheduled_tasks` / `task_runs` store module (M10).
//!
//! Mirrors `backend/app/tasks/service.py`: durable scheduled jobs plus their run
//! history/inbox. Schedule interpretation (cron, interval, date, quiet hours,
//! misfire/overlap) lives in [`crate::scheduler`]; this module owns persistence
//! and actor scoping.

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::domains::common::{
    bounded_limit, collect_rows, json_text, parse_json, query_one, user_id_for,
};
use crate::error::StoreError;
use crate::scheduler;
use crate::time::now_python;

/// Trigger kind: cron expression.
pub const TRIGGER_CRON: &str = "cron";
/// Trigger kind: fixed interval.
pub const TRIGGER_INTERVAL: &str = "interval";
/// Trigger kind: one-shot date.
pub const TRIGGER_DATE: &str = "date";

/// Misfire policy: execute however late.
pub const MISFIRE_RUN: &str = "run";
/// Misfire policy: drop a fire that is late beyond the grace window.
pub const MISFIRE_SKIP: &str = "skip";

/// Approval policy: external side effects are denied for the background run.
pub const APPROVAL_DENY_EXTERNAL: &str = "deny_external";
/// Approval policy: the user pre-approved external side effects.
pub const APPROVAL_ALLOW_ALL: &str = "allow_all";

/// Task run status: accepted but not yet executing.
pub const TASK_RUN_QUEUED: &str = "queued";
/// Task run status: executing.
pub const TASK_RUN_RUNNING: &str = "running";
/// Task run status: finished successfully.
pub const TASK_RUN_COMPLETED: &str = "completed";
/// Task run status: finished with an error.
pub const TASK_RUN_FAILED: &str = "failed";
/// Task run status: cancelled before completion.
pub const TASK_RUN_CANCELLED: &str = "cancelled";
/// Task run status: fired but deliberately not executed.
pub const TASK_RUN_SKIPPED: &str = "skipped";

/// Statuses that end a task run.
pub const TERMINAL_TASK_RUN_STATUSES: &[&str] = &[
    TASK_RUN_COMPLETED,
    TASK_RUN_FAILED,
    TASK_RUN_CANCELLED,
    TASK_RUN_SKIPPED,
];

/// A recurring or one-shot agent job definition.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledTask {
    pub id: i64,
    pub user_id: i64,
    pub name: String,
    pub description: Option<String>,
    pub trigger_type: String,
    pub cron_expression: Option<String>,
    pub interval_seconds: Option<i64>,
    pub run_at: Option<String>,
    pub timezone: String,
    pub quiet_hours_start: Option<String>,
    pub quiet_hours_end: Option<String>,
    pub misfire_policy: String,
    pub prompt: String,
    pub workflow_type: Option<String>,
    pub profile_id: Option<i64>,
    pub model: Option<String>,
    pub tools_whitelist: Option<Value>,
    pub capability_policy: Option<Value>,
    pub working_directory: Option<String>,
    pub approval_policy: String,
    pub delivery_channels: Option<Value>,
    pub delivery_config: Option<Value>,
    pub last_delivery_hash: Option<String>,
    pub max_iterations: i64,
    pub max_cost_per_run: Option<f64>,
    pub timeout_s: Option<f64>,
    pub enabled: bool,
    pub next_run_at: Option<String>,
    pub last_run_at: Option<String>,
    pub last_status: Option<String>,
    pub run_count: i64,
    pub failure_count: i64,
    pub created_at: String,
    pub updated_at: String,
}

impl ScheduledTask {
    fn from_row(row: &Row<'_>) -> Result<Self, StoreError> {
        Ok(Self {
            id: row.get("id")?,
            user_id: row.get("user_id")?,
            name: row.get("name")?,
            description: row.get("description")?,
            trigger_type: row.get("trigger_type")?,
            cron_expression: row.get("cron_expression")?,
            interval_seconds: row.get("interval_seconds")?,
            run_at: row.get("run_at")?,
            timezone: row.get("timezone")?,
            quiet_hours_start: row.get("quiet_hours_start")?,
            quiet_hours_end: row.get("quiet_hours_end")?,
            misfire_policy: row.get("misfire_policy")?,
            prompt: row.get("prompt")?,
            workflow_type: row.get("workflow_type")?,
            profile_id: row.get("profile_id")?,
            model: row.get("model")?,
            tools_whitelist: parse_json(row.get("tools_whitelist")?)?,
            capability_policy: parse_json(row.get("capability_policy")?)?,
            working_directory: row.get("working_directory")?,
            approval_policy: row.get("approval_policy")?,
            delivery_channels: parse_json(row.get("delivery_channels")?)?,
            delivery_config: parse_json(row.get("delivery_config")?)?,
            last_delivery_hash: row.get("last_delivery_hash")?,
            max_iterations: row.get("max_iterations")?,
            max_cost_per_run: row.get("max_cost_per_run")?,
            timeout_s: row.get("timeout_s")?,
            enabled: row.get::<_, i64>("enabled")? != 0,
            next_run_at: row.get("next_run_at")?,
            last_run_at: row.get("last_run_at")?,
            last_status: row.get("last_status")?,
            run_count: row.get("run_count")?,
            failure_count: row.get("failure_count")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

/// Create payload for a scheduled task, with the Python service defaults.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewScheduledTask {
    pub name: String,
    pub description: Option<String>,
    pub trigger_type: String,
    pub cron_expression: Option<String>,
    pub interval_seconds: Option<i64>,
    pub run_at: Option<String>,
    pub timezone: String,
    pub quiet_hours_start: Option<String>,
    pub quiet_hours_end: Option<String>,
    pub misfire_policy: String,
    pub prompt: String,
    pub workflow_type: Option<String>,
    pub profile_id: Option<i64>,
    pub model: Option<String>,
    pub tools_whitelist: Option<Value>,
    pub capability_policy: Option<Value>,
    pub working_directory: Option<String>,
    pub approval_policy: String,
    pub delivery_channels: Option<Value>,
    pub delivery_config: Option<Value>,
    pub max_iterations: i64,
    pub max_cost_per_run: Option<f64>,
    pub timeout_s: Option<f64>,
    pub enabled: bool,
}

impl Default for NewScheduledTask {
    fn default() -> Self {
        Self {
            name: String::new(),
            description: None,
            trigger_type: TRIGGER_CRON.to_string(),
            cron_expression: None,
            interval_seconds: None,
            run_at: None,
            timezone: "UTC".to_string(),
            quiet_hours_start: None,
            quiet_hours_end: None,
            // Python `ScheduledTask.misfire_policy` defaults to MISFIRE_SKIP.
            misfire_policy: MISFIRE_SKIP.to_string(),
            prompt: String::new(),
            workflow_type: None,
            profile_id: None,
            model: None,
            tools_whitelist: None,
            capability_policy: None,
            working_directory: None,
            // Python `create_task` defaults approval_policy to deny_external.
            approval_policy: APPROVAL_DENY_EXTERNAL.to_string(),
            delivery_channels: None,
            delivery_config: None,
            max_iterations: 10,
            max_cost_per_run: None,
            timeout_s: None,
            enabled: true,
        }
    }
}

/// Partial update payload; every field is optional.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledTaskPatch {
    pub name: Option<String>,
    pub description: Option<String>,
    pub trigger_type: Option<String>,
    pub cron_expression: Option<String>,
    pub interval_seconds: Option<i64>,
    pub run_at: Option<String>,
    pub timezone: Option<String>,
    pub quiet_hours_start: Option<String>,
    pub quiet_hours_end: Option<String>,
    pub misfire_policy: Option<String>,
    pub prompt: Option<String>,
    pub workflow_type: Option<String>,
    pub profile_id: Option<i64>,
    pub model: Option<String>,
    pub tools_whitelist: Option<Value>,
    pub capability_policy: Option<Value>,
    pub working_directory: Option<String>,
    pub approval_policy: Option<String>,
    pub delivery_channels: Option<Value>,
    pub delivery_config: Option<Value>,
    pub max_iterations: Option<i64>,
    pub max_cost_per_run: Option<f64>,
    pub timeout_s: Option<f64>,
    pub enabled: Option<bool>,
}

/// One execution attempt of a [`ScheduledTask`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskRun {
    pub id: i64,
    pub task_id: i64,
    pub conversation_id: Option<i64>,
    pub run_id: Option<i64>,
    pub status: String,
    pub trigger_source: String,
    pub prompt: String,
    pub output: Option<String>,
    pub error: Option<String>,
    pub skip_reason: Option<String>,
    pub approval_policy: Option<String>,
    pub approval_reason: Option<String>,
    pub usage: Option<Value>,
    pub duration_ms: Option<i64>,
    pub delivery_status: Option<Value>,
    pub delivered_at: Option<String>,
    pub is_read: bool,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl TaskRun {
    fn from_row(row: &Row<'_>) -> Result<Self, StoreError> {
        Ok(Self {
            id: row.get("id")?,
            task_id: row.get("task_id")?,
            conversation_id: row.get("conversation_id")?,
            run_id: row.get("run_id")?,
            status: row.get("status")?,
            trigger_source: row.get("trigger_source")?,
            prompt: row.get("prompt")?,
            output: row.get("output")?,
            error: row.get("error")?,
            skip_reason: row.get("skip_reason")?,
            approval_policy: row.get("approval_policy")?,
            approval_reason: row.get("approval_reason")?,
            usage: parse_json(row.get("usage")?)?,
            duration_ms: row.get("duration_ms")?,
            delivery_status: parse_json(row.get("delivery_status")?)?,
            delivered_at: row.get("delivered_at")?,
            is_read: row.get::<_, i64>("is_read")? != 0,
            started_at: row.get("started_at")?,
            finished_at: row.get("finished_at")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

/// Create payload for a task run (`started_at`/`created_at` default to now).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewTaskRun {
    pub trigger_source: String,
    pub prompt: String,
    pub status: String,
    pub approval_policy: Option<String>,
    pub approval_reason: Option<String>,
    pub skip_reason: Option<String>,
}

impl Default for NewTaskRun {
    fn default() -> Self {
        Self {
            trigger_source: "schedule".to_string(),
            prompt: String::new(),
            status: TASK_RUN_QUEUED.to_string(),
            approval_policy: None,
            approval_reason: None,
            skip_reason: None,
        }
    }
}

fn fetch_task(connection: &Connection, task_id: i64) -> Result<ScheduledTask, StoreError> {
    query_one(
        connection,
        "SELECT * FROM scheduled_tasks WHERE id = ?1",
        [task_id],
        ScheduledTask::from_row,
    )?
    .ok_or(StoreError::NotFound("scheduled task"))
}

fn require_task(
    connection: &Connection,
    actor_id: &str,
    task_id: i64,
) -> Result<ScheduledTask, StoreError> {
    let task = fetch_task(connection, task_id)?;
    if task.user_id != user_id_for(connection, actor_id)? {
        return Err(StoreError::NotFound("scheduled task"));
    }
    Ok(task)
}

fn fetch_task_run(connection: &Connection, run_id: i64) -> Result<TaskRun, StoreError> {
    query_one(
        connection,
        "SELECT * FROM task_runs WHERE id = ?1",
        [run_id],
        TaskRun::from_row,
    )?
    .ok_or(StoreError::NotFound("task run"))
}

fn require_task_run(
    connection: &Connection,
    actor_id: &str,
    run_id: i64,
) -> Result<TaskRun, StoreError> {
    let run = fetch_task_run(connection, run_id)?;
    // Scope the run through its owning task (task_runs has no user_id column).
    require_task(connection, actor_id, run.task_id)?;
    Ok(run)
}

/// The next fire time after `after` for a task, as a stored datetime string.
fn compute_next_run(task: &ScheduledTask, after: i64) -> Result<Option<String>, StoreError> {
    match scheduler::next_run(task, after) {
        Ok(Some(timestamp)) => Ok(Some(crate::time::python_datetime(timestamp, 0))),
        Ok(None) => Ok(None),
        Err(error) => Err(StoreError::InvalidInput(error.to_string())),
    }
}

/// Reject an unknown misfire/approval policy, like the Python service's
/// `ValueError` guards.
fn validate_policies(misfire_policy: &str, approval_policy: &str) -> Result<(), StoreError> {
    if !matches!(misfire_policy, MISFIRE_RUN | MISFIRE_SKIP) {
        return Err(StoreError::InvalidInput(format!(
            "unknown misfire_policy {misfire_policy:?}"
        )));
    }
    if !matches!(approval_policy, APPROVAL_DENY_EXTERNAL | APPROVAL_ALLOW_ALL) {
        return Err(StoreError::InvalidInput(format!(
            "unknown approval_policy {approval_policy:?}"
        )));
    }
    Ok(())
}

/// A throwaway task carrying only the schedule fields, used to validate a
/// create/patch before it touches the database (mirrors Python
/// `validate_schedule`, which runs before the write).
fn schedule_probe(
    trigger_type: String,
    cron_expression: Option<String>,
    interval_seconds: Option<i64>,
    run_at: Option<String>,
    timezone: String,
) -> ScheduledTask {
    ScheduledTask {
        id: 0,
        user_id: 0,
        name: String::new(),
        description: None,
        trigger_type,
        cron_expression,
        interval_seconds,
        run_at,
        timezone,
        quiet_hours_start: None,
        quiet_hours_end: None,
        misfire_policy: MISFIRE_SKIP.to_string(),
        prompt: String::new(),
        workflow_type: None,
        profile_id: None,
        model: None,
        tools_whitelist: None,
        capability_policy: None,
        working_directory: None,
        approval_policy: APPROVAL_DENY_EXTERNAL.to_string(),
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
        created_at: String::new(),
        updated_at: String::new(),
    }
}

impl crate::LegacyStore {
    /// Tasks owned by the actor, newest first.
    pub fn list_tasks(
        &self,
        actor_id: &str,
        enabled_only: bool,
    ) -> Result<Vec<ScheduledTask>, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        let mut statement = connection.prepare(
            "SELECT * FROM scheduled_tasks WHERE user_id = ?1 \
             AND (?2 = 0 OR enabled = 1) ORDER BY id DESC",
        )?;
        let rows = statement.query(params![user_id, i64::from(enabled_only)])?;
        collect_rows(rows, ScheduledTask::from_row)
    }

    /// Fetch one task, scoped to its owner.
    pub fn get_task(&self, actor_id: &str, task_id: i64) -> Result<ScheduledTask, StoreError> {
        let connection = self.connection()?;
        require_task(&connection, actor_id, task_id)
    }

    /// Create a task and compute its initial `next_run_at`.
    pub fn create_task(
        &self,
        actor_id: &str,
        new: &NewScheduledTask,
    ) -> Result<ScheduledTask, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        // Validate before writing, like Python `validate_schedule`.
        validate_policies(&new.misfire_policy, &new.approval_policy)?;
        scheduler::next_run(
            &schedule_probe(
                new.trigger_type.clone(),
                new.cron_expression.clone(),
                new.interval_seconds,
                new.run_at.clone(),
                new.timezone.clone(),
            ),
            now_seconds(),
        )?;
        let timestamp = now_python();
        connection.execute(
            "INSERT INTO scheduled_tasks(created_at, updated_at, user_id, name, description,
               trigger_type, cron_expression, interval_seconds, run_at, timezone, quiet_hours_start,
               quiet_hours_end, misfire_policy, prompt, workflow_type, profile_id, model,
               tools_whitelist, capability_policy, working_directory, approval_policy,
               delivery_channels, delivery_config, last_delivery_hash, max_iterations,
               max_cost_per_run, timeout_s, enabled, next_run_at, last_run_at, last_status,
               run_count, failure_count)
             VALUES (?1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
               ?17, ?18, ?19, ?20, ?21, ?22, NULL, ?23, ?24, ?25, ?26, NULL, NULL, NULL, 0, 0)",
            params![
                timestamp,
                user_id,
                new.name,
                new.description,
                new.trigger_type,
                new.cron_expression,
                new.interval_seconds,
                new.run_at,
                new.timezone,
                new.quiet_hours_start,
                new.quiet_hours_end,
                new.misfire_policy,
                new.prompt,
                new.workflow_type,
                new.profile_id,
                new.model,
                json_text(&new.tools_whitelist)?,
                json_text(&new.capability_policy)?,
                new.working_directory,
                new.approval_policy,
                json_text(&new.delivery_channels)?,
                json_text(&new.delivery_config)?,
                new.max_iterations,
                new.max_cost_per_run,
                new.timeout_s,
                i64::from(new.enabled),
            ],
        )?;
        let id = connection.last_insert_rowid();
        let mut task = fetch_task(&connection, id)?;
        if task.enabled {
            let next = compute_next_run(&task, now_seconds())?;
            connection.execute(
                "UPDATE scheduled_tasks SET next_run_at = ?1 WHERE id = ?2",
                params![next, id],
            )?;
            task.next_run_at = next;
        }
        Ok(task)
    }

    /// Patch a task; recomputes `next_run_at` when schedule fields or `enabled`
    /// change, matching `_SCHEDULE_FIELDS` in the Python service.
    pub fn update_task(
        &self,
        actor_id: &str,
        task_id: i64,
        patch: &ScheduledTaskPatch,
    ) -> Result<ScheduledTask, StoreError> {
        let connection = self.connection()?;
        let existing = require_task(&connection, actor_id, task_id)?;
        // Validate the post-patch schedule/policies before writing anything.
        if let Some(policy) = &patch.misfire_policy
            && !matches!(policy.as_str(), MISFIRE_RUN | MISFIRE_SKIP)
        {
            return Err(StoreError::InvalidInput(format!(
                "unknown misfire_policy {policy:?}"
            )));
        }
        if let Some(policy) = &patch.approval_policy
            && !matches!(policy.as_str(), APPROVAL_DENY_EXTERNAL | APPROVAL_ALLOW_ALL)
        {
            return Err(StoreError::InvalidInput(format!(
                "unknown approval_policy {policy:?}"
            )));
        }
        let schedule_touched = patch.trigger_type.is_some()
            || patch.cron_expression.is_some()
            || patch.interval_seconds.is_some()
            || patch.run_at.is_some()
            || patch.timezone.is_some()
            || patch.enabled.is_some();
        // Only validate/interpret the schedule when it (or enablement) changed;
        // a name-only patch must not fail on a pre-existing unsupported tz.
        if schedule_touched {
            scheduler::next_run(
                &schedule_probe(
                    patch
                        .trigger_type
                        .clone()
                        .unwrap_or_else(|| existing.trigger_type.clone()),
                    patch
                        .cron_expression
                        .clone()
                        .or_else(|| existing.cron_expression.clone()),
                    patch.interval_seconds.or(existing.interval_seconds),
                    patch.run_at.clone().or_else(|| existing.run_at.clone()),
                    patch
                        .timezone
                        .clone()
                        .unwrap_or_else(|| existing.timezone.clone()),
                ),
                now_seconds(),
            )?;
        }
        let mut assignments: Vec<&str> = Vec::new();
        let mut values: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(value) = &patch.name {
            assignments.push("name = ?");
            values.push(Box::new(value.clone()));
        }
        if let Some(value) = &patch.description {
            assignments.push("description = ?");
            values.push(Box::new(value.clone()));
        }
        if let Some(value) = &patch.trigger_type {
            assignments.push("trigger_type = ?");
            values.push(Box::new(value.clone()));
        }
        if let Some(value) = &patch.cron_expression {
            assignments.push("cron_expression = ?");
            values.push(Box::new(value.clone()));
        }
        if let Some(value) = patch.interval_seconds {
            assignments.push("interval_seconds = ?");
            values.push(Box::new(value));
        }
        if let Some(value) = &patch.run_at {
            assignments.push("run_at = ?");
            values.push(Box::new(value.clone()));
        }
        if let Some(value) = &patch.timezone {
            assignments.push("timezone = ?");
            values.push(Box::new(value.clone()));
        }
        if let Some(value) = &patch.quiet_hours_start {
            assignments.push("quiet_hours_start = ?");
            values.push(Box::new(value.clone()));
        }
        if let Some(value) = &patch.quiet_hours_end {
            assignments.push("quiet_hours_end = ?");
            values.push(Box::new(value.clone()));
        }
        if let Some(value) = &patch.misfire_policy {
            assignments.push("misfire_policy = ?");
            values.push(Box::new(value.clone()));
        }
        if let Some(value) = &patch.prompt {
            assignments.push("prompt = ?");
            values.push(Box::new(value.clone()));
        }
        if let Some(value) = &patch.workflow_type {
            assignments.push("workflow_type = ?");
            values.push(Box::new(value.clone()));
        }
        if let Some(value) = patch.profile_id {
            assignments.push("profile_id = ?");
            values.push(Box::new(value));
        }
        if let Some(value) = &patch.model {
            assignments.push("model = ?");
            values.push(Box::new(value.clone()));
        }
        if patch.tools_whitelist.is_some() {
            assignments.push("tools_whitelist = ?");
            values.push(Box::new(json_text(&patch.tools_whitelist)?));
        }
        if patch.capability_policy.is_some() {
            assignments.push("capability_policy = ?");
            values.push(Box::new(json_text(&patch.capability_policy)?));
        }
        if let Some(value) = &patch.working_directory {
            assignments.push("working_directory = ?");
            values.push(Box::new(value.clone()));
        }
        if let Some(value) = &patch.approval_policy {
            assignments.push("approval_policy = ?");
            values.push(Box::new(value.clone()));
        }
        if patch.delivery_channels.is_some() {
            assignments.push("delivery_channels = ?");
            values.push(Box::new(json_text(&patch.delivery_channels)?));
        }
        if patch.delivery_config.is_some() {
            assignments.push("delivery_config = ?");
            values.push(Box::new(json_text(&patch.delivery_config)?));
        }
        if let Some(value) = patch.max_iterations {
            assignments.push("max_iterations = ?");
            values.push(Box::new(value));
        }
        if let Some(value) = patch.max_cost_per_run {
            assignments.push("max_cost_per_run = ?");
            values.push(Box::new(value));
        }
        if let Some(value) = patch.timeout_s {
            assignments.push("timeout_s = ?");
            values.push(Box::new(value));
        }
        if let Some(value) = patch.enabled {
            assignments.push("enabled = ?");
            values.push(Box::new(i64::from(value)));
        }
        assignments.push("updated_at = ?");
        values.push(Box::new(now_python()));
        values.push(Box::new(task_id));
        let sql = format!(
            "UPDATE scheduled_tasks SET {} WHERE id = ?",
            assignments.join(", ")
        );
        let references: Vec<&dyn rusqlite::ToSql> =
            values.iter().map(|value| value.as_ref()).collect();
        connection.execute(&sql, references.as_slice())?;

        let mut task = fetch_task(&connection, task_id)?;
        if schedule_touched {
            let next = if task.enabled {
                compute_next_run(&task, now_seconds())?
            } else {
                None
            };
            connection.execute(
                "UPDATE scheduled_tasks SET next_run_at = ?1 WHERE id = ?2",
                params![next, task_id],
            )?;
            task.next_run_at = next;
        }
        Ok(task)
    }

    /// Delete a task and its runs.
    ///
    /// Mirrors the Python `session.delete` loop: task runs first, then the task.
    /// Foreign keys are off on both runtimes (the Python engine never enables
    /// `PRAGMA foreign_keys` and `cool-store` matches it), so deleting a task
    /// whose runs are referenced by `webhook_events.task_run_id` leaves the
    /// same dangling historical references as the Python service.
    pub fn delete_task(&self, actor_id: &str, task_id: i64) -> Result<(), StoreError> {
        let mut connection = self.connection()?;
        require_task(&connection, actor_id, task_id)?;
        let transaction = connection.transaction()?;
        transaction.execute("DELETE FROM task_runs WHERE task_id = ?1", [task_id])?;
        transaction.execute("DELETE FROM scheduled_tasks WHERE id = ?1", [task_id])?;
        transaction.commit()?;
        Ok(())
    }

    /// Enable or disable a task, recomputing `next_run_at`.
    pub fn set_task_enabled(
        &self,
        actor_id: &str,
        task_id: i64,
        enabled: bool,
    ) -> Result<ScheduledTask, StoreError> {
        let connection = self.connection()?;
        require_task(&connection, actor_id, task_id)?;
        connection.execute(
            "UPDATE scheduled_tasks SET enabled = ?1, updated_at = ?2 WHERE id = ?3",
            params![i64::from(enabled), now_python(), task_id],
        )?;
        let mut task = fetch_task(&connection, task_id)?;
        let next = if enabled {
            compute_next_run(&task, now_seconds())?
        } else {
            None
        };
        connection.execute(
            "UPDATE scheduled_tasks SET next_run_at = ?1 WHERE id = ?2",
            params![next, task_id],
        )?;
        task.next_run_at = next;
        Ok(task)
    }

    /// Service-level query used by the scheduler loop: every enabled task whose
    /// `next_run_at` is due. Not actor-scoped (the worker has no actor).
    pub fn list_due_tasks(&self, now: i64) -> Result<Vec<ScheduledTask>, StoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT * FROM scheduled_tasks WHERE enabled = 1 AND next_run_at IS NOT NULL \
             ORDER BY next_run_at, id",
        )?;
        let rows = statement.query([])?;
        let tasks = collect_rows(rows, ScheduledTask::from_row)?;
        Ok(tasks
            .into_iter()
            .filter(|task| {
                task.next_run_at
                    .as_deref()
                    .and_then(crate::time::parse_python_datetime)
                    .is_some_and(|due| due <= now)
            })
            .collect())
    }

    /// Record that a task fired: advance `next_run_at`, stamp `last_run_at` and
    /// mark it running. `next_run_at = None` leaves the value untouched.
    pub fn record_task_fired(
        &self,
        task_id: i64,
        next_run_at: Option<&str>,
    ) -> Result<(), StoreError> {
        let connection = self.connection()?;
        let timestamp = now_python();
        connection.execute(
            "UPDATE scheduled_tasks SET next_run_at = COALESCE(?1, next_run_at), \
               last_run_at = ?2, last_status = ?3, run_count = run_count + 1, updated_at = ?2 \
             WHERE id = ?4",
            params![next_run_at, timestamp, TASK_RUN_RUNNING, task_id],
        )?;
        Ok(())
    }

    /// Write the terminal state of a task run and roll the task's counters,
    /// mirroring Python `_finalize_run`: failures increment `failure_count` and
    /// auto-disable at the configured ceiling; successes reset it; a one-shot
    /// date task is disabled after any terminal run.
    pub fn record_task_outcome(
        &self,
        task_id: i64,
        status: &str,
        success: bool,
        max_consecutive_failures: u32,
    ) -> Result<ScheduledTask, StoreError> {
        let connection = self.connection()?;
        let task = fetch_task(&connection, task_id)?;
        let timestamp = now_python();
        let failure_count = if success { 0 } else { task.failure_count + 1 };
        let mut enabled = task.enabled;
        if !success
            && max_consecutive_failures > 0
            && failure_count >= i64::from(max_consecutive_failures)
        {
            enabled = false;
        }
        let terminal = TERMINAL_TASK_RUN_STATUSES.contains(&status);
        if task.trigger_type == TRIGGER_DATE && terminal {
            enabled = false;
        }
        connection.execute(
            "UPDATE scheduled_tasks SET last_status = ?1, last_run_at = ?2, \
               failure_count = ?3, enabled = ?4, updated_at = ?2 WHERE id = ?5",
            params![
                status,
                timestamp,
                failure_count,
                i64::from(enabled),
                task_id
            ],
        )?;
        let mut updated = fetch_task(&connection, task_id)?;
        let next = if updated.enabled {
            compute_next_run(&updated, now_seconds())?
        } else {
            None
        };
        connection.execute(
            "UPDATE scheduled_tasks SET next_run_at = ?1 WHERE id = ?2",
            params![next, task_id],
        )?;
        updated.next_run_at = next;
        Ok(updated)
    }

    /// Create a queued task-run row.
    pub fn create_task_run(
        &self,
        actor_id: &str,
        task_id: i64,
        new: &NewTaskRun,
    ) -> Result<TaskRun, StoreError> {
        let connection = self.connection()?;
        require_task(&connection, actor_id, task_id)?;
        let timestamp = now_python();
        connection.execute(
            "INSERT INTO task_runs(created_at, updated_at, task_id, conversation_id, run_id, status,
               trigger_source, prompt, output, error, skip_reason, approval_policy, approval_reason,
               usage, duration_ms, delivery_status, delivered_at, is_read, started_at, finished_at)
             VALUES (?1, ?1, ?2, NULL, NULL, ?3, ?4, ?5, NULL, NULL, ?6, ?7, ?8, NULL, NULL, NULL,
               NULL, 0, ?1, NULL)",
            params![
                timestamp,
                task_id,
                new.status,
                new.trigger_source,
                new.prompt,
                new.skip_reason,
                new.approval_policy,
                new.approval_reason,
            ],
        )?;
        let id = connection.last_insert_rowid();
        fetch_task_run(&connection, id)
    }

    /// Finish a task run with its terminal output/usage.
    #[allow(clippy::too_many_arguments)]
    pub fn finish_task_run(
        &self,
        actor_id: &str,
        run_id: i64,
        status: &str,
        output: Option<&str>,
        error: Option<&str>,
        usage: Option<&Value>,
        duration_ms: Option<i64>,
        conversation_id: Option<i64>,
        agent_run_id: Option<i64>,
    ) -> Result<TaskRun, StoreError> {
        let connection = self.connection()?;
        require_task_run(&connection, actor_id, run_id)?;
        let timestamp = now_python();
        connection.execute(
            "UPDATE task_runs SET status = ?1, output = ?2, error = ?3, \
               usage = COALESCE(?4, usage), duration_ms = COALESCE(?5, duration_ms), \
               conversation_id = COALESCE(?6, conversation_id), run_id = COALESCE(?7, run_id), \
               finished_at = ?8, updated_at = ?8 WHERE id = ?9",
            params![
                status,
                output,
                error,
                usage.map(serde_json::to_string).transpose()?,
                duration_ms,
                conversation_id,
                agent_run_id,
                timestamp,
                run_id,
            ],
        )?;
        fetch_task_run(&connection, run_id)
    }

    /// Record a fire time that deliberately did not execute.
    ///
    /// Mirrors `record_skipped_run` in the Python service: the run row is
    /// informational (`is_read = 1`) and the task's `next_run_at` moves forward
    /// so a skip-policy task does not stay stuck in the past.
    pub fn record_skipped_run(
        &self,
        actor_id: &str,
        task_id: i64,
        reason: &str,
    ) -> Result<TaskRun, StoreError> {
        let mut connection = self.connection()?;
        let task = require_task(&connection, actor_id, task_id)?;
        let timestamp = now_python();
        let next_run = if task.enabled {
            compute_next_run(&task, now_seconds())?
        } else {
            None
        };
        let transaction = connection.transaction()?;
        transaction.execute(
            "INSERT INTO task_runs(created_at, updated_at, task_id, conversation_id, run_id, status,
               trigger_source, prompt, output, error, skip_reason, approval_policy, approval_reason,
               usage, duration_ms, delivery_status, delivered_at, is_read, started_at, finished_at)
             VALUES (?1, ?1, ?2, NULL, NULL, ?3, 'schedule', ?4, NULL, NULL, ?5, NULL, NULL, NULL,
               NULL, NULL, NULL, 1, ?1, ?1)",
            params![timestamp, task_id, TASK_RUN_SKIPPED, task.prompt, reason],
        )?;
        let id = transaction.last_insert_rowid();
        transaction.execute(
            "UPDATE scheduled_tasks SET next_run_at = ?1, last_run_at = ?2, last_status = ?3,
               updated_at = ?2 WHERE id = ?4",
            params![next_run, timestamp, TASK_RUN_SKIPPED, task_id],
        )?;
        transaction.commit()?;
        fetch_task_run(&connection, id)
    }

    /// Runs for a task, newest first.
    pub fn list_task_runs(
        &self,
        actor_id: &str,
        task_id: i64,
        limit: Option<usize>,
    ) -> Result<Vec<TaskRun>, StoreError> {
        let connection = self.connection()?;
        require_task(&connection, actor_id, task_id)?;
        let mut statement = connection
            .prepare("SELECT * FROM task_runs WHERE task_id = ?1 ORDER BY id DESC LIMIT ?2")?;
        let rows = statement.query(params![task_id, bounded_limit(limit, 50, 500)])?;
        collect_rows(rows, TaskRun::from_row)
    }

    /// Fetch one run, scoped through its owning task.
    pub fn get_task_run(&self, actor_id: &str, run_id: i64) -> Result<TaskRun, StoreError> {
        let connection = self.connection()?;
        require_task_run(&connection, actor_id, run_id)
    }

    /// Inbox view across all of the actor's tasks, newest first.
    pub fn list_task_inbox(
        &self,
        actor_id: &str,
        unread_only: bool,
        limit: Option<usize>,
    ) -> Result<Vec<TaskRun>, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        let mut statement = connection.prepare(
            "SELECT * FROM task_runs WHERE task_id IN \
               (SELECT id FROM scheduled_tasks WHERE user_id = ?1) \
             AND (?2 = 0 OR is_read = 0) ORDER BY created_at DESC, id DESC LIMIT ?3",
        )?;
        let rows = statement.query(params![
            user_id,
            i64::from(unread_only),
            bounded_limit(limit, 100, 500),
        ])?;
        collect_rows(rows, TaskRun::from_row)
    }

    /// Number of unread inbox runs across the actor's tasks.
    pub fn count_unread_task_runs(&self, actor_id: &str) -> Result<i64, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        let count = connection.query_row(
            "SELECT COUNT(*) FROM task_runs WHERE is_read = 0 AND task_id IN \
               (SELECT id FROM scheduled_tasks WHERE user_id = ?1)",
            [user_id],
            |row| row.get(0),
        )?;
        Ok(count)
    }

    /// Flip a run's inbox read state.
    pub fn mark_task_run_read(
        &self,
        actor_id: &str,
        run_id: i64,
        read: bool,
    ) -> Result<TaskRun, StoreError> {
        let connection = self.connection()?;
        require_task_run(&connection, actor_id, run_id)?;
        connection.execute(
            "UPDATE task_runs SET is_read = ?1, updated_at = ?2 WHERE id = ?3",
            params![i64::from(read), now_python(), run_id],
        )?;
        fetch_task_run(&connection, run_id)
    }

    /// Mark an in-flight run cancelled. A run already in a terminal state is
    /// returned unchanged (Python `cancel_task_run` returns `False`).
    pub fn cancel_task_run(&self, actor_id: &str, run_id: i64) -> Result<TaskRun, StoreError> {
        let connection = self.connection()?;
        let run = require_task_run(&connection, actor_id, run_id)?;
        if TERMINAL_TASK_RUN_STATUSES.contains(&run.status.as_str()) {
            return Ok(run);
        }
        let timestamp = now_python();
        connection.execute(
            "UPDATE task_runs SET status = ?1, finished_at = ?2, updated_at = ?2 WHERE id = ?3",
            params![TASK_RUN_CANCELLED, timestamp, run_id],
        )?;
        fetch_task_run(&connection, run_id)
    }
}

/// Current wall-clock time as unix seconds (used for initial next-run math).
fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
}

/// Task owner (diagnostics/tests).
pub fn task_owner(connection: &Connection, task_id: i64) -> Result<Option<i64>, StoreError> {
    Ok(connection
        .query_row(
            "SELECT user_id FROM scheduled_tasks WHERE id = ?1",
            [task_id],
            |row| row.get(0),
        )
        .optional()?)
}
