//! Background scheduler parity with the Python APScheduler runtime (M10).
//!
//! The engine is deterministic and clock-injectable so restart, catch-up,
//! misfire and overlap behavior can be tested without wall-clock waits.
//!
//! Scope decisions for M10:
//!
//! - Only the standard 5-field cron form is supported
//!   (`minute hour day-of-month month day-of-week`) with `*`, lists, ranges and
//!   steps, including 3-letter month/day names. This matches the shape the
//!   Python API/UI produce (`backend/app/tasks/cron.py`).
//! - Timezones are limited to UTC (case-insensitive), `GMT`, and fixed-offset
//!   strings (`+HH:MM` or `-HHMM`). There is no IANA tz database in the Rust
//!   core yet, so an IANA name such as `Europe/Berlin` is a hard
//!   [`ScheduleError::UnsupportedTimezone`] — it never silently degrades to UTC
//!   the way the Python `resolve_timezone` fallback does for unknown names.
//!   Python maps the same fixed offsets to UTC instead, so this is a documented
//!   extension, not exact parity.
//! - Day-of-month and day-of-week combine with cron's OR rule when both are
//!   restricted, matching `croniter(..., day_or=True)`, the Python default.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::domains::tasks::{
    MISFIRE_RUN, ScheduledTask, TRIGGER_CRON, TRIGGER_DATE, TRIGGER_INTERVAL,
};
use crate::error::StoreError;
use crate::time::{civil_date, iso_weekday, parse_python_datetime, python_datetime};

/// How far in the future a cron search scans before giving up (8 years covers a
/// leap-day-only schedule with room to spare).
const MAX_CRON_DAYS: i64 = 366 * 8;

const MONTH_NAMES: [&str; 12] = [
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
];

const DAY_NAMES: [&str; 7] = [
    "sunday",
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
];

/// Runtime tuning for the deterministic scheduler engine.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SchedulerConfig {
    /// Seconds a late fire may still run before the misfire policy applies.
    pub misfire_grace_seconds: i64,
    /// Consecutive failures after which a task is auto-disabled. `0` = never.
    pub max_consecutive_failures: u32,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            misfire_grace_seconds: 300,
            max_consecutive_failures: 5,
        }
    }
}

/// A normalized trigger definition resolved from a [`ScheduledTask`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Trigger {
    /// Standard 5-field cron expression.
    Cron(String),
    /// Fixed interval in seconds.
    Interval(i64),
    /// One-shot fire time (UTC unix seconds).
    Date(i64),
}

impl Trigger {
    /// Resolve the trigger a task describes, failing closed on bad input.
    pub fn from_task(task: &ScheduledTask) -> Result<Self, ScheduleError> {
        match task.trigger_type.as_str() {
            TRIGGER_CRON => {
                let expression = task
                    .cron_expression
                    .clone()
                    .ok_or(ScheduleError::MissingCronExpression)?;
                Ok(Self::Cron(expression))
            }
            TRIGGER_INTERVAL => {
                let seconds = task
                    .interval_seconds
                    .ok_or(ScheduleError::InvalidInterval(0))?;
                if seconds < 1 {
                    return Err(ScheduleError::InvalidInterval(seconds));
                }
                Ok(Self::Interval(seconds))
            }
            TRIGGER_DATE => {
                let text = task.run_at.clone().ok_or(ScheduleError::MissingRunAt)?;
                let run_at = parse_python_datetime(&text)
                    .ok_or_else(|| ScheduleError::InvalidRunAt(text.clone()))?;
                Ok(Self::Date(run_at))
            }
            other => Err(ScheduleError::UnknownTrigger(other.to_string())),
        }
    }
}

/// Errors from schedule parsing and next-run computation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScheduleError {
    /// The task names a timezone the Rust core cannot interpret.
    UnsupportedTimezone(String),
    /// The cron expression is malformed.
    InvalidCron(String),
    /// A cron trigger has no expression.
    MissingCronExpression,
    /// An interval trigger has no (or a non-positive) interval.
    InvalidInterval(i64),
    /// A date trigger has no `run_at`.
    MissingRunAt,
    /// `run_at` is not a parseable datetime.
    InvalidRunAt(String),
    /// The task uses a trigger type the engine does not know.
    UnknownTrigger(String),
    /// No occurrence exists within the search horizon.
    NoFutureOccurrence(String),
}

impl std::fmt::Display for ScheduleError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedTimezone(name) => {
                write!(
                    formatter,
                    "unsupported timezone {name:?} (UTC or fixed offset required)"
                )
            }
            Self::InvalidCron(message) => write!(formatter, "invalid cron expression: {message}"),
            Self::MissingCronExpression => formatter.write_str("cron trigger needs an expression"),
            Self::InvalidInterval(seconds) => {
                write!(
                    formatter,
                    "interval trigger needs a positive interval, got {seconds}"
                )
            }
            Self::MissingRunAt => formatter.write_str("date trigger needs a run_at"),
            Self::InvalidRunAt(value) => write!(formatter, "invalid run_at {value:?}"),
            Self::UnknownTrigger(trigger) => write!(formatter, "unknown trigger type {trigger:?}"),
            Self::NoFutureOccurrence(expression) => {
                write!(formatter, "no future occurrence for {expression:?}")
            }
        }
    }
}

impl std::error::Error for ScheduleError {}

impl From<ScheduleError> for StoreError {
    fn from(error: ScheduleError) -> Self {
        Self::InvalidInput(error.to_string())
    }
}

/// Next fire time (UTC unix seconds) strictly after `after`, or `None` when the
/// task will never fire again (a consumed one-shot date trigger).
pub fn next_run(task: &ScheduledTask, after: i64) -> Result<Option<i64>, ScheduleError> {
    let offset = timezone_offset(&task.timezone)?;
    match Trigger::from_task(task)? {
        Trigger::Cron(expression) => Ok(Some(next_cron_after(&expression, offset, after)?)),
        Trigger::Interval(seconds) => Ok(Some(after + seconds)),
        Trigger::Date(run_at) => Ok(if run_at > after { Some(run_at) } else { None }),
    }
}

/// Whether `moment` (UTC unix seconds) falls inside the task's quiet-hours
/// window. Missing, malformed or equal bounds mean "no quiet hours".
pub fn quiet_hours(task: &ScheduledTask, moment: i64) -> Result<bool, ScheduleError> {
    // Match Python `in_quiet_hours`: missing/invalid bounds short-circuit to
    // "no quiet hours" before the timezone is ever consulted.
    let (Some(start), Some(end)) = (
        parse_hhmm(task.quiet_hours_start.as_deref()),
        parse_hhmm(task.quiet_hours_end.as_deref()),
    ) else {
        return Ok(false);
    };
    if start == end {
        return Ok(false);
    }
    let offset = timezone_offset(&task.timezone)?;
    let local_minutes = (moment + offset).rem_euclid(86_400) / 60;
    let inside = if start < end {
        start <= local_minutes && local_minutes < end
    } else {
        local_minutes >= start || local_minutes < end
    };
    Ok(inside)
}

/// One scheduling action for a due task.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Decision {
    /// Run the task once now (coalesced), recording `scheduled_for`.
    Execute { task_id: i64, scheduled_for: i64 },
    /// Deliberately do nothing this tick.
    Skip { task_id: i64, reason: String },
}

/// Deterministic, thread-free scheduler engine.
///
/// `running` tracks tasks with an in-flight execution so `max_instances=1`
/// overlap suppression works; `pending` remembers the fire time each in-flight
/// execution is serving.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Scheduler {
    config: SchedulerConfig,
    running: BTreeSet<i64>,
    pending: BTreeMap<i64, i64>,
}

impl Scheduler {
    /// Create an engine with explicit tuning.
    pub fn new(config: SchedulerConfig) -> Self {
        Self {
            config,
            running: BTreeSet::new(),
            pending: BTreeMap::new(),
        }
    }

    /// The effective configuration.
    pub fn config(&self) -> &SchedulerConfig {
        &self.config
    }

    /// Tasks currently marked as running.
    pub fn running(&self) -> &BTreeSet<i64> {
        &self.running
    }

    /// Decide what to do for each due task at `now` (UTC unix seconds).
    ///
    /// A task is due when it is enabled and `next_run_at <= now`. Missed fires
    /// coalesce into a single `Execute` whose `scheduled_for` is the stored due
    /// timestamp. A task already running is skipped as `"overlap"`; a late fire
    /// on a `skip`-policy task beyond the grace window is skipped as `"missed"`.
    pub fn plan(
        &mut self,
        tasks: &[ScheduledTask],
        now: i64,
    ) -> Result<Vec<Decision>, ScheduleError> {
        let mut decisions = Vec::new();
        for task in tasks {
            if !task.enabled {
                continue;
            }
            let Some(due) = task.next_run_at.as_deref().and_then(parse_python_datetime) else {
                continue;
            };
            if due > now {
                continue;
            }
            if quiet_hours(task, now)? {
                decisions.push(Decision::Skip {
                    task_id: task.id,
                    reason: format!(
                        "quiet hours {} - {}",
                        task.quiet_hours_start.as_deref().unwrap_or_default(),
                        task.quiet_hours_end.as_deref().unwrap_or_default()
                    ),
                });
                continue;
            }
            if self.running.contains(&task.id) {
                decisions.push(Decision::Skip {
                    task_id: task.id,
                    reason: "overlap".to_string(),
                });
                continue;
            }
            let late = now - due;
            if task.misfire_policy != MISFIRE_RUN && late > self.config.misfire_grace_seconds {
                decisions.push(Decision::Skip {
                    task_id: task.id,
                    reason: format!(
                        "missed fire time {} (late by {late}s)",
                        python_datetime(due, 0)
                    ),
                });
                continue;
            }
            self.running.insert(task.id);
            self.pending.insert(task.id, due);
            decisions.push(Decision::Execute {
                task_id: task.id,
                scheduled_for: due,
            });
        }
        Ok(decisions)
    }

    /// Clear a task's in-flight state once its run has finished.
    pub fn complete(&mut self, task_id: i64) {
        self.running.remove(&task_id);
        self.pending.remove(&task_id);
    }
}

/// Resolve a timezone name to a fixed offset in seconds east of UTC.
fn timezone_offset(name: &str) -> Result<i64, ScheduleError> {
    let trimmed = name.trim();
    if trimmed.is_empty()
        || trimmed.eq_ignore_ascii_case("utc")
        || trimmed.eq_ignore_ascii_case("gmt")
    {
        return Ok(0);
    }
    let rest = strip_prefix_ci(trimmed, "utc")
        .or_else(|| strip_prefix_ci(trimmed, "gmt"))
        .unwrap_or(trimmed);
    parse_offset(rest).ok_or_else(|| ScheduleError::UnsupportedTimezone(name.to_string()))
}

fn strip_prefix_ci<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let head = text.get(..prefix.len())?;
    if head.eq_ignore_ascii_case(prefix) {
        Some(&text[prefix.len()..])
    } else {
        None
    }
}

fn parse_offset(text: &str) -> Option<i64> {
    let (sign, rest) = match text.as_bytes().first()? {
        b'+' => (1_i64, text[1..].trim()),
        b'-' => (-1_i64, text[1..].trim()),
        _ => return None,
    };
    let (hours_text, minutes_text) = match rest.split_once(':') {
        Some((hours, minutes)) => {
            // Documented grammar is `+HH:MM`; a single-digit hour would make
            // the accepted set looser than the documented extension.
            if hours.len() != 2 || !hours.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            (hours, minutes)
        }
        None if rest.len() == 4 && rest.is_ascii() => (&rest[..2], &rest[2..]),
        None => return None,
    };
    let hours: i64 = hours_text.parse().ok()?;
    let minutes: i64 = minutes_text.parse().ok()?;
    if !(0..=23).contains(&hours) || !(0..=59).contains(&minutes) {
        return None;
    }
    Some(sign * (hours * 3_600 + minutes * 60))
}

fn parse_hhmm(value: Option<&str>) -> Option<i64> {
    let text = value?.trim();
    let (hours_text, minutes_text) = match text.split_once(':') {
        Some((hours, minutes)) => {
            // Python's `_parse_hhmm` requires exactly two minute digits and
            // returns None for e.g. "9:5"; accept the same grammar.
            if minutes.len() != 2 || !minutes.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            (hours, minutes)
        }
        None => (text, "0"),
    };
    if hours_text.is_empty()
        || hours_text.len() > 2
        || !hours_text.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let hours: i64 = hours_text.parse().ok()?;
    let minutes: i64 = minutes_text.parse().ok()?;
    if !(0..=23).contains(&hours) || !(0..=59).contains(&minutes) {
        return None;
    }
    Some(hours * 60 + minutes)
}

#[derive(Clone, Copy)]
enum NameField {
    None,
    Month,
    Day,
}

struct CronSpec {
    minutes: Vec<bool>,
    hours: Vec<bool>,
    days: Vec<bool>,
    months: Vec<bool>,
    weekdays: Vec<bool>,
    dom_restricted: bool,
    dow_restricted: bool,
}

fn parse_cron(expression: &str) -> Result<CronSpec, ScheduleError> {
    let fields: Vec<&str> = expression.split_whitespace().collect();
    if fields.len() != 5 {
        return Err(ScheduleError::InvalidCron(format!(
            "expected 5 fields, got {} in {expression:?}",
            fields.len()
        )));
    }
    let (minutes, _) = parse_field(fields[0], 0, 59, NameField::None)?;
    let (hours, _) = parse_field(fields[1], 0, 23, NameField::None)?;
    let (days, dom_restricted) = parse_field(fields[2], 1, 31, NameField::None)?;
    let (months, _) = parse_field(fields[3], 1, 12, NameField::Month)?;
    let (raw_weekdays, dow_restricted) = parse_field(fields[4], 0, 7, NameField::Day)?;

    // cron numbers day-of-week 0-7 (both 0 and 7 are Sunday); the time helpers
    // report ISO weekdays Monday=0..Sunday=6.
    let mut weekdays = vec![false; 7];
    for value in 0..=6_i64 {
        if raw_weekdays[value as usize] {
            weekdays[((value + 6) % 7) as usize] = true;
        }
    }
    if raw_weekdays[7] {
        weekdays[6] = true;
    }

    Ok(CronSpec {
        minutes,
        hours,
        days,
        months,
        weekdays,
        dom_restricted,
        dow_restricted,
    })
}

fn parse_field(
    field: &str,
    min: i64,
    max: i64,
    kind: NameField,
) -> Result<(Vec<bool>, bool), ScheduleError> {
    let mut set = vec![false; (max + 1) as usize];
    let restricted = field.trim() != "*";
    for part in field.split(',') {
        let part = part.trim();
        if part.is_empty() {
            return Err(ScheduleError::InvalidCron(format!(
                "empty field in {field:?}"
            )));
        }
        let (range_text, step) = match part.split_once('/') {
            Some((range_text, step_text)) => {
                let step: i64 = step_text
                    .parse()
                    .map_err(|_| ScheduleError::InvalidCron(format!("bad step {step_text:?}")))?;
                if step < 1 {
                    return Err(ScheduleError::InvalidCron(format!(
                        "bad step {step_text:?}"
                    )));
                }
                (range_text, step)
            }
            None => (part, 1),
        };
        let (start, end) = if range_text == "*" {
            (min, max)
        } else if let Some((start_text, end_text)) = range_text.split_once('-') {
            (parse_value(start_text, kind)?, parse_value(end_text, kind)?)
        } else {
            let value = parse_value(range_text, kind)?;
            if part.contains('/') {
                (value, max)
            } else {
                (value, value)
            }
        };
        if start < min || end > max || start > end {
            return Err(ScheduleError::InvalidCron(format!(
                "out-of-range field {field:?}"
            )));
        }
        let mut value = start;
        while value <= end {
            set[value as usize] = true;
            value += step;
        }
    }
    Ok((set, restricted))
}

fn parse_value(token: &str, kind: NameField) -> Result<i64, ScheduleError> {
    if let Ok(value) = token.parse::<i64>() {
        return Ok(value);
    }
    let lower = token.to_ascii_lowercase();
    let (names, base): (&[&str], i64) = match kind {
        NameField::Month => (&MONTH_NAMES, 1),
        NameField::Day => (&DAY_NAMES, 0),
        NameField::None => {
            return Err(ScheduleError::InvalidCron(format!(
                "unknown value {token:?}"
            )));
        }
    };
    names
        .iter()
        .position(|name| lower.len() >= 3 && name.starts_with(&lower))
        .map(|index| index as i64 + base)
        .ok_or_else(|| ScheduleError::InvalidCron(format!("unknown name {token:?}")))
}

fn next_cron_after(expression: &str, offset: i64, after: i64) -> Result<i64, ScheduleError> {
    let spec = parse_cron(expression)?;
    // Work in local civil time; cron fields are local-time based. The first
    // candidate is the next whole minute strictly after `after`.
    let mut current = (after + offset).div_euclid(60) * 60 + 60;
    for _ in 0..MAX_CRON_DAYS {
        let days = current.div_euclid(86_400);
        let (year, month, day) = civil_date(days);
        if day_matches(&spec, year, month, day) {
            let second_of_day = current.rem_euclid(86_400);
            let start_hour = second_of_day / 3_600;
            let start_minute = (second_of_day % 3_600) / 60;
            for (hour, hour_ok) in spec.hours.iter().enumerate() {
                if !hour_ok || (hour as i64) < start_hour {
                    continue;
                }
                let minute_floor = if hour as i64 == start_hour {
                    start_minute
                } else {
                    0
                };
                for (minute, minute_ok) in spec.minutes.iter().enumerate() {
                    if !minute_ok || (minute as i64) < minute_floor {
                        continue;
                    }
                    let local = days * 86_400 + hour as i64 * 3_600 + minute as i64 * 60;
                    return Ok(local - offset);
                }
            }
        }
        current = (days + 1) * 86_400;
    }
    Err(ScheduleError::NoFutureOccurrence(expression.to_string()))
}

fn day_matches(spec: &CronSpec, year: i64, month: u32, day: u32) -> bool {
    if !spec.months[month as usize] {
        return false;
    }
    let dom_ok = spec.days[day as usize];
    let weekday = iso_weekday(year, month, day) as usize;
    let dow_ok = spec.weekdays[weekday];
    match (spec.dom_restricted, spec.dow_restricted) {
        (true, true) => dom_ok || dow_ok,
        (true, false) => dom_ok,
        (false, true) => dow_ok,
        (false, false) => true,
    }
}
