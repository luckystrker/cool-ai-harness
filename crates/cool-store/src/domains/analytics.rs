//! Analytics aggregations over spend/tool/run/memory tables (M10).
//!
//! Read-only projections matching `backend/app/api/analytics.py` +
//! `backend/app/analytics/__init__.py`. Every query is actor-scoped through the
//! owning `user_id` (single-user MVP today, multi-actor correct for the new
//! protocol). Timestamps are SQLite strings, so buckets come from `substr`.
//!
// NEEDS(PY): Python's analytics functions are unscoped (single-user MVP); this
// store additionally filters by the acting user's `user_id`, which is a no-op
// while one user owns every row.
// NEEDS(PY): `call_history` returns the rows only; [`LegacyStore::call_history_total`]
// exists for the API's `total` field.

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::domains::common::{bounded_limit, collect_rows, user_id_for};
use crate::error::StoreError;
use crate::time::python_datetime;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalyticsSummary {
    pub total_spend_usd: f64,
    pub total_llm_calls: i64,
    pub total_tokens: i64,
    pub total_tool_calls: i64,
    pub tool_error_count: i64,
    pub tool_success_rate: f64,
    pub days: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpendBucket {
    pub period: String,
    pub cost_usd: f64,
    pub total_tokens: i64,
    pub calls: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelSpend {
    pub model: String,
    pub cost_usd: f64,
    pub total_tokens: i64,
    pub calls: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolUsage {
    pub name: String,
    pub calls: i64,
    pub avg_duration_ms: f64,
    pub success_rate: f64,
    pub error_count: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LatencyBucket {
    pub period: String,
    pub avg_ms: f64,
    pub min_ms: i64,
    pub max_ms: i64,
    pub calls: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallHistoryRow {
    pub id: i64,
    pub ts: Option<String>,
    pub model: String,
    pub provider_name: String,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    pub cost_usd: f64,
    pub run_id: Option<i64>,
    pub conversation_id: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryActivityBucket {
    pub period: String,
    pub created: i64,
    pub by_type: BTreeMap<String, i64>,
}

fn round(value: f64, digits: i32) -> f64 {
    let factor = 10f64.powi(digits);
    (value * factor).round() / factor
}

/// Lower bound timestamp string for a `days`-long lookback window.
fn cutoff(days: i64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    python_datetime(now - days.max(0) * 86_400, 0)
}

/// SQL expression formatting a timestamp column into a Python-compatible
/// bucket label: day → `YYYY-MM-DD`, hour → `YYYY-MM-DD HH:00` (matching
/// `strftime('%Y-%m-%d %H:00', ...)` in `backend/app/analytics/`).
fn period_expression(column: &str, bucket: &str) -> Result<String, StoreError> {
    match bucket {
        "day" => Ok(format!("substr({column}, 1, 10)")),
        "hour" => Ok(format!("substr({column}, 1, 13) || ':00'")),
        other => Err(StoreError::InvalidInput(format!(
            "bucket must be 'day' or 'hour', got {other:?}"
        ))),
    }
}

impl crate::LegacyStore {
    /// High-level summary for the analytics overview card.
    pub fn summary(&self, actor_id: &str, days: i64) -> Result<AnalyticsSummary, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        let since = cutoff(days);
        let (spend_usd, calls, tokens): (f64, i64, i64) = connection.query_row(
            "SELECT COALESCE(SUM(cost_usd), 0.0), COUNT(*), COALESCE(SUM(total_tokens), 0)
             FROM spend_log WHERE user_id = ?1 AND ts >= ?2",
            params![user_id, since],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        let (tool_calls, tool_success): (i64, i64) = connection.query_row(
            "SELECT COUNT(*), COALESCE(SUM(CASE WHEN success = 1 THEN 1 ELSE 0 END), 0)
             FROM tool_calls WHERE user_id = ?1 AND created_at >= ?2",
            params![user_id, since],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let tool_errors = tool_calls - tool_success;
        let success_rate = if tool_calls > 0 {
            round((tool_calls - tool_errors) as f64 / tool_calls as f64, 3)
        } else {
            1.0
        };
        Ok(AnalyticsSummary {
            total_spend_usd: round(spend_usd, 6),
            total_llm_calls: calls,
            total_tokens: tokens,
            total_tool_calls: tool_calls,
            tool_error_count: tool_errors,
            tool_success_rate: success_rate,
            days,
        })
    }

    /// Spend aggregated into day/hour buckets, ordered by period.
    pub fn spend_over_time(
        &self,
        actor_id: &str,
        days: i64,
        bucket: &str,
    ) -> Result<Vec<SpendBucket>, StoreError> {
        let period = period_expression("ts", bucket)?;
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        let since = cutoff(days);
        let sql = format!(
            "SELECT {period} AS period, COALESCE(SUM(cost_usd), 0.0),
               COALESCE(SUM(total_tokens), 0), COUNT(*)
             FROM spend_log WHERE user_id = ?1 AND ts >= ?2
             GROUP BY period ORDER BY period"
        );
        let mut statement = connection.prepare(&sql)?;
        let rows = statement.query(params![user_id, since])?;
        collect_rows(rows, |row| {
            Ok(SpendBucket {
                period: row.get::<_, Option<String>>(0)?.unwrap_or_default(),
                cost_usd: round(row.get(1)?, 6),
                total_tokens: row.get(2)?,
                calls: row.get(3)?,
            })
        })
    }

    /// Spend grouped by model, highest cost first.
    pub fn spend_by_model(&self, actor_id: &str, days: i64) -> Result<Vec<ModelSpend>, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        let since = cutoff(days);
        let mut statement = connection.prepare(
            "SELECT model, COALESCE(SUM(cost_usd), 0.0), COALESCE(SUM(total_tokens), 0), COUNT(*)
             FROM spend_log WHERE user_id = ?1 AND ts >= ?2
             GROUP BY model ORDER BY SUM(cost_usd) DESC",
        )?;
        let rows = statement.query(params![user_id, since])?;
        collect_rows(rows, |row| {
            let model: Option<String> = row.get(0)?;
            Ok(ModelSpend {
                model: model
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(|| "unknown".to_string()),
                cost_usd: round(row.get(1)?, 6),
                total_tokens: row.get(2)?,
                calls: row.get(3)?,
            })
        })
    }

    /// Most-used tools with success rate and average duration.
    pub fn top_tools(
        &self,
        actor_id: &str,
        days: i64,
        limit: usize,
    ) -> Result<Vec<ToolUsage>, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        let since = cutoff(days);
        let mut statement = connection.prepare(
            "SELECT name, COUNT(*), COALESCE(AVG(duration_ms), 0.0),
               COALESCE(SUM(CASE WHEN success = 1 THEN 1 ELSE 0 END), 0)
             FROM tool_calls WHERE user_id = ?1 AND created_at >= ?2
             GROUP BY name ORDER BY COUNT(*) DESC, name ASC LIMIT ?3",
        )?;
        let rows =
            statement.query(params![user_id, since, bounded_limit(Some(limit), 20, 100),])?;
        collect_rows(rows, |row| {
            let calls: i64 = row.get(1)?;
            let success: i64 = row.get(3)?;
            Ok(ToolUsage {
                name: row.get(0)?,
                calls,
                avg_duration_ms: round(row.get(2)?, 1),
                success_rate: if calls > 0 {
                    round(success as f64 / calls as f64, 3)
                } else {
                    0.0
                },
                error_count: calls - success,
            })
        })
    }

    /// LLM call latency from `llm_call_complete` run events, by bucket.
    pub fn latency(
        &self,
        actor_id: &str,
        days: i64,
        bucket: &str,
    ) -> Result<Vec<LatencyBucket>, StoreError> {
        let period = period_expression("e.created_at", bucket)?;
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        let since = cutoff(days);
        let sql = format!(
            "SELECT {period} AS period,
               COALESCE(AVG(json_extract(e.payload, '$.duration_ms')), 0.0),
               COALESCE(MIN(json_extract(e.payload, '$.duration_ms')), 0.0),
               COALESCE(MAX(json_extract(e.payload, '$.duration_ms')), 0.0),
               COUNT(*)
             FROM run_events e JOIN agent_runs r ON e.run_id = r.id
             WHERE e.kind = 'llm_call_complete' AND r.user_id = ?1 AND e.created_at >= ?2
             GROUP BY period ORDER BY period"
        );
        let mut statement = connection.prepare(&sql)?;
        let rows = statement.query(params![user_id, since])?;
        collect_rows(rows, |row| {
            Ok(LatencyBucket {
                period: row.get::<_, Option<String>>(0)?.unwrap_or_default(),
                avg_ms: round(row.get(1)?, 1),
                min_ms: row.get::<_, Option<f64>>(2)?.unwrap_or(0.0) as i64,
                max_ms: row.get::<_, Option<f64>>(3)?.unwrap_or(0.0) as i64,
                calls: row.get(4)?,
            })
        })
    }

    /// Unified LLM call log with pagination and optional filters.
    pub fn call_history(
        &self,
        actor_id: &str,
        limit: usize,
        offset: usize,
        model: Option<&str>,
        provider: Option<&str>,
    ) -> Result<Vec<CallHistoryRow>, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        let mut statement = connection.prepare(
            "SELECT * FROM spend_log WHERE user_id = ?1
               AND (?2 IS NULL OR model = ?2) AND (?3 IS NULL OR provider_name = ?3)
             ORDER BY ts DESC, id DESC LIMIT ?4 OFFSET ?5",
        )?;
        let rows = statement.query(params![
            user_id,
            model,
            provider,
            bounded_limit(Some(limit), 100, 1_000),
            offset as i64,
        ])?;
        collect_rows(rows, |row| {
            Ok(CallHistoryRow {
                id: row.get("id")?,
                ts: row.get("ts")?,
                model: row.get("model")?,
                provider_name: row.get("provider_name")?,
                prompt_tokens: row.get("prompt_tokens")?,
                completion_tokens: row.get("completion_tokens")?,
                total_tokens: row.get("total_tokens")?,
                cost_usd: row.get("cost_usd")?,
                run_id: row.get("run_id")?,
                conversation_id: row.get("conversation_id")?,
            })
        })
    }

    /// Row count backing [`Self::call_history`] pagination.
    pub fn call_history_total(
        &self,
        actor_id: &str,
        model: Option<&str>,
        provider: Option<&str>,
    ) -> Result<i64, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        let total: i64 = connection.query_row(
            "SELECT COUNT(*) FROM spend_log WHERE user_id = ?1
               AND (?2 IS NULL OR model = ?2) AND (?3 IS NULL OR provider_name = ?3)",
            params![user_id, model, provider],
            |row| row.get(0),
        )?;
        Ok(total)
    }

    /// Memory item creation activity over time, with per-type breakdown.
    pub fn memory_activity(
        &self,
        actor_id: &str,
        days: i64,
        bucket: &str,
    ) -> Result<Vec<MemoryActivityBucket>, StoreError> {
        let period = period_expression("created_at", bucket)?;
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        let since = cutoff(days);
        let sql = format!(
            "SELECT {period} AS period, memory_type, COUNT(*)
             FROM memory_items WHERE user_id = ?1 AND created_at >= ?2
             GROUP BY period, memory_type ORDER BY period"
        );
        let mut statement = connection.prepare(&sql)?;
        let rows = statement.query(params![user_id, since])?;
        let mut periods: BTreeMap<String, MemoryActivityBucket> = BTreeMap::new();
        for row in collect_rows::<(String, Option<String>, i64)>(rows, |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })? {
            let (period, memory_type, count) = row;
            let bucket = periods
                .entry(period.clone())
                .or_insert_with(|| MemoryActivityBucket {
                    period,
                    created: 0,
                    by_type: BTreeMap::new(),
                });
            bucket.created += count;
            let key = memory_type
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "unknown".to_string());
            *bucket.by_type.entry(key).or_insert(0) += count;
        }
        Ok(periods.into_values().collect())
    }
}
