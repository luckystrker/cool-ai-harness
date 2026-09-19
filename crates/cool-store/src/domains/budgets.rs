//! Legacy `budgets` / `spend_log` store module (M10).
//!
//! Mirrors `backend/app/budgets/service.py`: one budget row per user, created
//! lazily with the configured defaults (80 % alert threshold, block on
//! exceed), plus an append-only spend log queried by window.
//!
// NEEDS(PY): Python's `get_budget` lazily creates the default row; this
// contract's `get_budget` is a pure read returning `None`. Creation happens in
// `upsert_budget` / `set_budget_override` / `touch_budget_alert`.
// NEEDS(PY): `log_spend` validates that a supplied run/conversation belongs to
// the actor (Python relies on foreign keys being off and does not check).

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};

use crate::domains::common::{bounded_limit, collect_rows, query_one, user_id_for};
use crate::error::StoreError;
use crate::time::{now_python, python_datetime};

/// Defaults copied from `config.budget_alert_threshold_pct`.
pub const DEFAULT_ALERT_THRESHOLD_PCT: f64 = 80.0;
/// Defaults copied from `config.budget_block_on_exceed`.
pub const DEFAULT_BLOCK_ON_EXCEED: bool = true;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Budget {
    pub id: i64,
    pub user_id: i64,
    pub daily_limit_usd: Option<f64>,
    pub weekly_limit_usd: Option<f64>,
    pub monthly_limit_usd: Option<f64>,
    pub alert_threshold_pct: f64,
    pub block_on_exceed: bool,
    pub override_until: Option<String>,
    pub last_alert_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl Budget {
    fn from_row(row: &Row<'_>) -> Result<Self, StoreError> {
        Ok(Self {
            id: row.get("id")?,
            user_id: row.get("user_id")?,
            daily_limit_usd: row.get("daily_limit_usd")?,
            weekly_limit_usd: row.get("weekly_limit_usd")?,
            monthly_limit_usd: row.get("monthly_limit_usd")?,
            alert_threshold_pct: row.get("alert_threshold_pct")?,
            block_on_exceed: row.get::<_, i64>("block_on_exceed")? != 0,
            override_until: row.get("override_until")?,
            last_alert_at: row.get("last_alert_at")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BudgetUpdate {
    pub daily_limit_usd: Option<f64>,
    pub weekly_limit_usd: Option<f64>,
    pub monthly_limit_usd: Option<f64>,
    pub alert_threshold_pct: Option<f64>,
    pub block_on_exceed: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpendEntry {
    pub id: i64,
    pub run_id: Option<i64>,
    pub conversation_id: Option<i64>,
    pub provider_name: String,
    pub model: String,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    pub cost_usd: f64,
    pub ts: String,
    pub created_at: String,
    pub updated_at: String,
}

impl SpendEntry {
    fn from_row(row: &Row<'_>) -> Result<Self, StoreError> {
        Ok(Self {
            id: row.get("id")?,
            run_id: row.get("run_id")?,
            conversation_id: row.get("conversation_id")?,
            provider_name: row.get("provider_name")?,
            model: row.get("model")?,
            prompt_tokens: row.get("prompt_tokens")?,
            completion_tokens: row.get("completion_tokens")?,
            total_tokens: row.get("total_tokens")?,
            cost_usd: row.get("cost_usd")?,
            ts: row.get("ts")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewSpendEntry {
    pub run_id: Option<i64>,
    pub conversation_id: Option<i64>,
    pub provider_name: String,
    pub model: String,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    pub cost_usd: f64,
    pub ts: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpendSummary {
    pub calls: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    pub cost_usd: f64,
}

fn fetch_budget(connection: &Connection, user_id: i64) -> Result<Option<Budget>, StoreError> {
    query_one(
        connection,
        "SELECT * FROM budgets WHERE user_id = ?1",
        [user_id],
        Budget::from_row,
    )
}

/// Create the default budget row when the user has none (Python `get_budget`).
fn ensure_budget(connection: &Connection, user_id: i64) -> Result<(), StoreError> {
    let existing: Option<i64> = connection
        .query_row(
            "SELECT id FROM budgets WHERE user_id = ?1",
            [user_id],
            |row| row.get(0),
        )
        .optional()?;
    if existing.is_none() {
        let timestamp = now_python();
        connection.execute(
            "INSERT INTO budgets(created_at, updated_at, user_id, alert_threshold_pct,
               block_on_exceed) VALUES (?1, ?1, ?2, ?3, ?4)",
            params![
                timestamp,
                user_id,
                DEFAULT_ALERT_THRESHOLD_PCT,
                i64::from(DEFAULT_BLOCK_ON_EXCEED),
            ],
        )?;
    }
    Ok(())
}

fn validate_update(update: &BudgetUpdate) -> Result<(), StoreError> {
    for (name, value) in [
        ("daily_limit_usd", update.daily_limit_usd),
        ("weekly_limit_usd", update.weekly_limit_usd),
        ("monthly_limit_usd", update.monthly_limit_usd),
        ("alert_threshold_pct", update.alert_threshold_pct),
    ] {
        if let Some(value) = value
            && value < 0.0
        {
            return Err(StoreError::InvalidInput(format!("{name} must be >= 0")));
        }
    }
    if let Some(threshold) = update.alert_threshold_pct
        && threshold > 100.0
    {
        return Err(StoreError::InvalidInput(
            "alert_threshold_pct must be <= 100".to_string(),
        ));
    }
    Ok(())
}

fn required_budget(connection: &Connection, user_id: i64) -> Result<Budget, StoreError> {
    fetch_budget(connection, user_id)?
        .ok_or(StoreError::Corruption("budget row disappeared".to_string()))
}

/// Spend against one budget window, mirroring `security.cost.WindowSpend`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BudgetWindowStatus {
    pub spend_usd: f64,
    pub limit_usd: Option<f64>,
    pub pct: f64,
}

/// Live budget picture mirroring `security.cost.BudgetEvaluation`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BudgetStatus {
    pub status: String,
    pub overridden: bool,
    pub daily: BudgetWindowStatus,
    pub weekly: BudgetWindowStatus,
    pub monthly: BudgetWindowStatus,
    pub daily_limit_usd: Option<f64>,
    pub weekly_limit_usd: Option<f64>,
    pub monthly_limit_usd: Option<f64>,
    pub alert_threshold_pct: f64,
    pub block_on_exceed: bool,
    pub override_until: Option<String>,
}

/// UTC start of the period containing `now`, matching `_window_start`.
fn window_start(window: &str, now: i64) -> i64 {
    let day_start = crate::time::start_of_day(now);
    match window {
        "daily" => day_start,
        "weekly" => {
            let (year, month, day) = crate::time::civil_date(now.div_euclid(86_400));
            day_start - i64::from(crate::time::iso_weekday(year, month, day)) * 86_400
        }
        _ => {
            let (year, month, _) = crate::time::civil_date(now.div_euclid(86_400));
            crate::time::days_from_civil(year, month, 1) * 86_400
        }
    }
}

impl crate::LegacyStore {
    /// The actor's budget row, or `None` when it was never created.
    pub fn get_budget(&self, actor_id: &str) -> Result<Option<Budget>, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        fetch_budget(&connection, user_id)
    }

    /// Create-or-update the actor's budget row; only provided fields change.
    pub fn upsert_budget(
        &self,
        actor_id: &str,
        update: &BudgetUpdate,
    ) -> Result<Budget, StoreError> {
        validate_update(update)?;
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        ensure_budget(&connection, user_id)?;
        let mut assignments: Vec<&str> = Vec::new();
        let mut values: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(value) = update.daily_limit_usd {
            assignments.push("daily_limit_usd = ?");
            values.push(Box::new(value));
        }
        if let Some(value) = update.weekly_limit_usd {
            assignments.push("weekly_limit_usd = ?");
            values.push(Box::new(value));
        }
        if let Some(value) = update.monthly_limit_usd {
            assignments.push("monthly_limit_usd = ?");
            values.push(Box::new(value));
        }
        if let Some(value) = update.alert_threshold_pct {
            assignments.push("alert_threshold_pct = ?");
            values.push(Box::new(value));
        }
        if let Some(value) = update.block_on_exceed {
            assignments.push("block_on_exceed = ?");
            values.push(Box::new(i64::from(value)));
        }
        assignments.push("updated_at = ?");
        values.push(Box::new(now_python()));
        values.push(Box::new(user_id));
        let sql = format!(
            "UPDATE budgets SET {} WHERE user_id = ?",
            assignments.join(", ")
        );
        let references: Vec<&dyn rusqlite::ToSql> =
            values.iter().map(|value| value.as_ref()).collect();
        connection.execute(&sql, references.as_slice())?;
        required_budget(&connection, user_id)
    }

    /// Set (`Some`) or clear (`None`) the block override.
    pub fn set_budget_override(
        &self,
        actor_id: &str,
        until: Option<&str>,
    ) -> Result<Budget, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        ensure_budget(&connection, user_id)?;
        connection.execute(
            "UPDATE budgets SET override_until = ?1, updated_at = ?2 WHERE user_id = ?3",
            params![until, now_python(), user_id],
        )?;
        required_budget(&connection, user_id)
    }

    /// Record that an alert fired at `at` (debounces repeat alerts).
    pub fn touch_budget_alert(&self, actor_id: &str, at: &str) -> Result<(), StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        ensure_budget(&connection, user_id)?;
        connection.execute(
            "UPDATE budgets SET last_alert_at = ?1, updated_at = ?2 WHERE user_id = ?3",
            params![at, now_python(), user_id],
        )?;
        Ok(())
    }

    /// Append one spend row; returns its id.
    pub fn log_spend(&self, actor_id: &str, entry: &NewSpendEntry) -> Result<i64, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        if let Some(conversation_id) = entry.conversation_id {
            crate::domains::common::require_conversation(&connection, actor_id, conversation_id)?;
        }
        if let Some(run_id) = entry.run_id {
            let conversation: Option<i64> = connection
                .query_row(
                    "SELECT conversation_id FROM agent_runs WHERE id = ?1",
                    [run_id],
                    |row| row.get(0),
                )
                .optional()?;
            match conversation {
                Some(conversation_id) => {
                    crate::domains::common::require_conversation(
                        &connection,
                        actor_id,
                        conversation_id,
                    )?;
                }
                None => return Err(StoreError::NotFound("run")),
            }
        }
        let timestamp = now_python();
        let ts = entry.ts.clone().unwrap_or_else(|| timestamp.clone());
        connection.execute(
            "INSERT INTO spend_log(created_at, updated_at, user_id, run_id, conversation_id,
               provider_name, model, prompt_tokens, completion_tokens, total_tokens, cost_usd, ts)
             VALUES (?1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                timestamp,
                user_id,
                entry.run_id,
                entry.conversation_id,
                entry.provider_name,
                entry.model,
                entry.prompt_tokens,
                entry.completion_tokens,
                entry.total_tokens,
                entry.cost_usd,
                ts,
            ],
        )?;
        Ok(connection.last_insert_rowid())
    }

    /// Recent spend rows, newest first. `since` is a unix timestamp compared
    /// against the stored `ts` string.
    pub fn list_spend(
        &self,
        actor_id: &str,
        since: Option<i64>,
        limit: Option<usize>,
    ) -> Result<Vec<SpendEntry>, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        let since_text = since.map(|value| python_datetime(value, 0));
        let mut statement = connection.prepare(
            "SELECT * FROM spend_log WHERE user_id = ?1 AND (?2 IS NULL OR ts >= ?2) \
             ORDER BY ts DESC, id DESC LIMIT ?3",
        )?;
        let rows = statement.query(params![
            user_id,
            since_text,
            bounded_limit(limit, 200, 1_000),
        ])?;
        collect_rows(rows, SpendEntry::from_row)
    }

    /// Aggregate spend totals for the actor over an optional window.
    pub fn spend_summary(
        &self,
        actor_id: &str,
        since: Option<i64>,
    ) -> Result<SpendSummary, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        let since_text = since.map(|value| python_datetime(value, 0));
        let summary = query_one(
            &connection,
            "SELECT COUNT(*), COALESCE(SUM(prompt_tokens), 0), COALESCE(SUM(completion_tokens), 0),
               COALESCE(SUM(total_tokens), 0), COALESCE(SUM(cost_usd), 0.0)
             FROM spend_log WHERE user_id = ?1 AND (?2 IS NULL OR ts >= ?2)",
            params![user_id, since_text],
            |row| {
                Ok(SpendSummary {
                    calls: row.get(0)?,
                    prompt_tokens: row.get(1)?,
                    completion_tokens: row.get(2)?,
                    total_tokens: row.get(3)?,
                    cost_usd: row.get(4)?,
                })
            },
        )?
        .ok_or_else(|| StoreError::Corruption("spend aggregate returned no row".to_string()))?;
        Ok(SpendSummary {
            cost_usd: (summary.cost_usd * 1_000_000.0).round() / 1_000_000.0,
            ..summary
        })
    }

    /// Full budget picture for the actor at `now` (unix seconds), mirroring
    /// `budgets.budget_evaluation` including calendar window starts and the
    /// override/alert/block status decision.
    pub fn budget_status(&self, actor_id: &str, now: i64) -> Result<BudgetStatus, StoreError> {
        let budget = self.get_budget(actor_id)?;
        let status = |window: &str, limit: Option<f64>| -> Result<BudgetWindowStatus, StoreError> {
            let summary = self.spend_summary(actor_id, Some(window_start(window, now)))?;
            let pct = match limit {
                Some(limit) if limit > 0.0 => summary.cost_usd / limit * 100.0,
                _ => 0.0,
            };
            Ok(BudgetWindowStatus {
                spend_usd: summary.cost_usd,
                limit_usd: limit,
                pct: (pct * 100.0).round() / 100.0,
            })
        };
        let (daily_limit, weekly_limit, monthly_limit, alert_threshold_pct, block_on_exceed) =
            match &budget {
                Some(budget) => (
                    budget.daily_limit_usd,
                    budget.weekly_limit_usd,
                    budget.monthly_limit_usd,
                    budget.alert_threshold_pct,
                    budget.block_on_exceed,
                ),
                None => (
                    None,
                    None,
                    None,
                    DEFAULT_ALERT_THRESHOLD_PCT,
                    DEFAULT_BLOCK_ON_EXCEED,
                ),
            };
        let override_until = budget
            .as_ref()
            .and_then(|budget| budget.override_until.clone());
        let overridden = override_until
            .as_deref()
            .and_then(crate::time::parse_python_datetime)
            .is_some_and(|until| until > now);
        let daily = status("daily", daily_limit)?;
        let weekly = status("weekly", weekly_limit)?;
        let monthly = status("monthly", monthly_limit)?;
        let alerted = |window: &BudgetWindowStatus| {
            window.limit_usd.is_some() && window.pct >= alert_threshold_pct
        };
        let exceeded = |window: &BudgetWindowStatus| {
            window.limit_usd.is_some() && window.spend_usd >= window.limit_usd.unwrap_or_default()
        };
        let any_exceeded = exceeded(&daily) || exceeded(&weekly) || exceeded(&monthly);
        let any_alerted = alerted(&daily) || alerted(&weekly) || alerted(&monthly);
        let status = if any_exceeded && block_on_exceed && !overridden {
            "blocked"
        } else if any_alerted {
            "alert"
        } else {
            "ok"
        };
        Ok(BudgetStatus {
            status: status.to_owned(),
            overridden,
            daily,
            weekly,
            monthly,
            daily_limit_usd: daily_limit,
            weekly_limit_usd: weekly_limit,
            monthly_limit_usd: monthly_limit,
            alert_threshold_pct,
            block_on_exceed,
            override_until,
        })
    }
}
