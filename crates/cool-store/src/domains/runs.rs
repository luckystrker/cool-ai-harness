//! Runs, run events, tool calls and approval audits
//! (legacy `agent_runs`, `run_events`, `tool_calls`, `approval_audits`).

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::domains::common::{
    bounded_limit, collect_rows, json_text, parse_json, query_one, require_conversation,
    user_id_for,
};
use crate::error::StoreError;
use crate::time::now_python;

pub const RUN_STATUSES: &[&str] = &[
    "queued",
    "running",
    "awaiting_approval",
    "completed",
    "failed",
    "cancelled",
];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRun {
    pub id: i64,
    pub conversation_id: i64,
    pub user_id: Option<i64>,
    pub status: String,
    pub model: Option<String>,
    pub config: Option<Value>,
    pub checkpoint: Option<Value>,
    pub usage: Option<Value>,
    pub iterations: i64,
    pub finish_reason: Option<String>,
    pub error: Option<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl AgentRun {
    fn from_row(row: &Row<'_>) -> Result<Self, StoreError> {
        Ok(Self {
            id: row.get("id")?,
            conversation_id: row.get("conversation_id")?,
            user_id: row.get("user_id")?,
            status: row.get("status")?,
            model: row.get("model")?,
            config: parse_json(row.get("config")?)?,
            checkpoint: parse_json(row.get("checkpoint")?)?,
            usage: parse_json(row.get("usage")?)?,
            iterations: row.get("iterations")?,
            finish_reason: row.get("finish_reason")?,
            error: row.get("error")?,
            started_at: row.get("started_at")?,
            finished_at: row.get("finished_at")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

/// Fetch a run and validate actor ownership without re-locking the store.
fn fetch_run(connection: &Connection, actor_id: &str, run_id: i64) -> Result<AgentRun, StoreError> {
    let run = query_one(
        connection,
        "SELECT * FROM agent_runs WHERE id = ?1",
        [run_id],
        AgentRun::from_row,
    )?
    .ok_or(StoreError::NotFound("run"))?;
    require_conversation(connection, actor_id, run.conversation_id)?;
    Ok(run)
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewRun {
    pub model: Option<String>,
    pub config: Option<Value>,
}

#[derive(Clone, Debug, Default)]
pub struct RunFilter {
    pub before_id: Option<i64>,
    pub limit: Option<usize>,
}

impl crate::LegacyStore {
    pub fn list_runs(
        &self,
        actor_id: &str,
        conversation_id: i64,
        filter: &RunFilter,
    ) -> Result<Vec<AgentRun>, StoreError> {
        let connection = self.connection()?;
        require_conversation(&connection, actor_id, conversation_id)?;
        let mut statement = connection.prepare(
            "SELECT * FROM agent_runs WHERE conversation_id = ?1 \
             AND (?2 IS NULL OR id < ?2) ORDER BY id DESC LIMIT ?3",
        )?;
        let rows = statement.query(params![
            conversation_id,
            filter.before_id,
            bounded_limit(filter.limit, 50, 500),
        ])?;
        collect_rows(rows, AgentRun::from_row)
    }

    pub fn get_run(&self, actor_id: &str, run_id: i64) -> Result<AgentRun, StoreError> {
        let connection = self.connection()?;
        fetch_run(&connection, actor_id, run_id)
    }

    pub fn create_run(
        &self,
        actor_id: &str,
        conversation_id: i64,
        new: &NewRun,
    ) -> Result<AgentRun, StoreError> {
        let connection = self.connection()?;
        let user_id = require_conversation(&connection, actor_id, conversation_id)?;
        let timestamp = now_python();
        connection.execute(
            "INSERT INTO agent_runs(created_at, updated_at, conversation_id, user_id, status,
               model, config, iterations, started_at)
             VALUES (?1, ?1, ?2, ?3, 'running', ?4, ?5, 0, ?1)",
            params![
                timestamp,
                conversation_id,
                user_id,
                new.model,
                json_text(&new.config)?,
            ],
        )?;
        let id = connection.last_insert_rowid();
        drop(connection);
        self.get_run(actor_id, id)
    }

    /// Finish a run, validating the Python run-status vocabulary.
    #[allow(clippy::too_many_arguments)]
    pub fn finish_run(
        &self,
        actor_id: &str,
        run_id: i64,
        status: &str,
        usage: Option<&Value>,
        iterations: Option<i64>,
        finish_reason: Option<&str>,
        error: Option<&str>,
    ) -> Result<AgentRun, StoreError> {
        if !matches!(status, "completed" | "failed" | "cancelled") {
            return Err(StoreError::InvalidInput(format!(
                "run status {status:?} is not terminal"
            )));
        }
        let connection = self.connection()?;
        let run = fetch_run(&connection, actor_id, run_id)?;
        let owner = user_id_for(&connection, actor_id)?;
        if run.user_id.is_some_and(|user_id| user_id != owner) {
            return Err(StoreError::NotFound("run"));
        }
        let timestamp = now_python();
        connection.execute(
            "UPDATE agent_runs SET status = ?1, usage = COALESCE(?2, usage),
               iterations = COALESCE(?3, iterations), finish_reason = ?4, error = ?5,
               finished_at = ?6, updated_at = ?6 WHERE id = ?7",
            params![
                status,
                usage.map(serde_json::to_string).transpose()?,
                iterations,
                finish_reason,
                error,
                timestamp,
                run_id,
            ],
        )?;
        drop(connection);
        self.get_run(actor_id, run_id)
    }

    /// Update the durable checkpoint of a run.
    pub fn set_run_checkpoint(
        &self,
        actor_id: &str,
        run_id: i64,
        checkpoint: &Value,
    ) -> Result<(), StoreError> {
        let connection = self.connection()?;
        fetch_run(&connection, actor_id, run_id)?;
        connection.execute(
            "UPDATE agent_runs SET checkpoint = ?1, updated_at = ?2 WHERE id = ?3",
            params![serde_json::to_string(checkpoint)?, now_python(), run_id],
        )?;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunEvent {
    pub id: i64,
    pub run_id: i64,
    pub seq: i64,
    pub kind: String,
    pub payload: Option<Value>,
    pub created_at: String,
}

impl RunEvent {
    fn from_row(row: &Row<'_>) -> Result<Self, StoreError> {
        Ok(Self {
            id: row.get("id")?,
            run_id: row.get("run_id")?,
            seq: row.get("seq")?,
            kind: row.get("kind")?,
            payload: parse_json(row.get("payload")?)?,
            created_at: row.get("created_at")?,
        })
    }
}

impl crate::LegacyStore {
    /// Append a run event with the next per-run sequence number.
    ///
    /// Python allocates the first event at `seq = 0` (`(-1 if current_max is
    /// None else current_max) + 1` in `app/agent/service.py`), so the Rust
    /// store must start at zero too or streaming cursors diverge.
    pub fn append_run_event(
        &self,
        actor_id: &str,
        run_id: i64,
        kind: &str,
        payload: Option<&Value>,
    ) -> Result<RunEvent, StoreError> {
        let mut connection = self.connection()?;
        fetch_run(&connection, actor_id, run_id)?;
        let timestamp = now_python();
        let transaction = connection.transaction()?;
        let next_seq: i64 = transaction.query_row(
            "SELECT COALESCE(MAX(seq), -1) + 1 FROM run_events WHERE run_id = ?1",
            [run_id],
            |row| row.get(0),
        )?;
        transaction.execute(
            "INSERT INTO run_events(created_at, updated_at, run_id, seq, kind, payload)
             VALUES (?1, ?1, ?2, ?3, ?4, ?5)",
            params![
                timestamp,
                run_id,
                next_seq,
                kind,
                payload.map(serde_json::to_string).transpose()?,
            ],
        )?;
        let id = transaction.last_insert_rowid();
        transaction.commit()?;
        drop(connection);
        Ok(RunEvent {
            id,
            run_id,
            seq: next_seq,
            kind: kind.to_string(),
            payload: payload.cloned(),
            created_at: timestamp,
        })
    }

    pub fn list_run_events(
        &self,
        actor_id: &str,
        run_id: i64,
        after_seq: Option<i64>,
        limit: Option<usize>,
    ) -> Result<Vec<RunEvent>, StoreError> {
        let connection = self.connection()?;
        fetch_run(&connection, actor_id, run_id)?;
        let mut statement = connection.prepare(
            "SELECT * FROM run_events WHERE run_id = ?1 AND (?2 IS NULL OR seq > ?2) \
             ORDER BY seq LIMIT ?3",
        )?;
        let rows = statement.query(params![
            run_id,
            after_seq,
            bounded_limit(limit, 1_000, 10_000),
        ])?;
        collect_rows(rows, RunEvent::from_row)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallRecord {
    pub id: i64,
    pub conversation_id: Option<i64>,
    pub message_id: Option<i64>,
    pub user_id: Option<i64>,
    pub name: String,
    pub arguments: Option<Value>,
    pub result: Option<Value>,
    pub duration_ms: Option<i64>,
    pub success: bool,
    pub error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl ToolCallRecord {
    fn from_row(row: &Row<'_>) -> Result<Self, StoreError> {
        Ok(Self {
            id: row.get("id")?,
            conversation_id: row.get("conversation_id")?,
            message_id: row.get("message_id")?,
            user_id: row.get("user_id")?,
            name: row.get("name")?,
            arguments: parse_json(row.get("arguments")?)?,
            result: parse_json(row.get("result")?)?,
            duration_ms: row.get("duration_ms")?,
            success: row.get::<_, i64>("success")? != 0,
            error: row.get("error")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewToolCall {
    pub conversation_id: Option<i64>,
    pub message_id: Option<i64>,
    pub name: String,
    pub arguments: Option<Value>,
    pub result: Option<Value>,
    pub duration_ms: Option<i64>,
    pub success: bool,
    pub error: Option<String>,
}

impl crate::LegacyStore {
    pub fn record_tool_call(
        &self,
        actor_id: &str,
        new: &NewToolCall,
    ) -> Result<ToolCallRecord, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        if let Some(conversation_id) = new.conversation_id {
            require_conversation(&connection, actor_id, conversation_id)?;
        }
        let timestamp = now_python();
        connection.execute(
            "INSERT INTO tool_calls(created_at, updated_at, conversation_id, message_id, user_id,
               name, arguments, result, duration_ms, success, error)
             VALUES (?1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                timestamp,
                new.conversation_id,
                new.message_id,
                user_id,
                new.name,
                json_text(&new.arguments)?,
                json_text(&new.result)?,
                new.duration_ms,
                i64::from(new.success),
                new.error,
            ],
        )?;
        let id = connection.last_insert_rowid();
        drop(connection);
        self.get_tool_call(actor_id, id)
    }

    /// Fetch a tool call, scoped to its owner.
    ///
    /// Rows with a conversation must belong to the actor; service-level rows
    /// without one are checked against `tool_calls.user_id`.
    pub fn get_tool_call(
        &self,
        actor_id: &str,
        tool_call_id: i64,
    ) -> Result<ToolCallRecord, StoreError> {
        let connection = self.connection()?;
        let record = query_one(
            &connection,
            "SELECT * FROM tool_calls WHERE id = ?1",
            [tool_call_id],
            ToolCallRecord::from_row,
        )?
        .ok_or(StoreError::NotFound("tool call"))?;
        if let Some(conversation_id) = record.conversation_id {
            require_conversation(&connection, actor_id, conversation_id)?;
        } else {
            // Service-level rows without a conversation must still be owned by
            // this actor; legacy rows with no owner are not readable at all.
            let user_id = user_id_for(&connection, actor_id)?;
            if record.user_id != Some(user_id) {
                return Err(StoreError::NotFound("tool call"));
            }
        }
        Ok(record)
    }

    pub fn list_tool_calls(
        &self,
        actor_id: &str,
        conversation_id: Option<i64>,
        limit: Option<usize>,
    ) -> Result<Vec<ToolCallRecord>, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        if let Some(conversation_id) = conversation_id {
            require_conversation(&connection, actor_id, conversation_id)?;
        }
        let mut statement = connection.prepare(
            "SELECT * FROM tool_calls WHERE user_id = ?1 AND (?2 IS NULL OR conversation_id = ?2) \
             ORDER BY id DESC LIMIT ?3",
        )?;
        let rows = statement.query(params![
            user_id,
            conversation_id,
            bounded_limit(limit, 100, 1_000),
        ])?;
        collect_rows(rows, ToolCallRecord::from_row)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalAudit {
    pub id: i64,
    pub conversation_id: i64,
    pub run_id: Option<i64>,
    pub call_id: String,
    pub tool_name: String,
    pub arguments: Option<Value>,
    pub approved: bool,
    pub decision_source: String,
    pub decided_by: Option<String>,
    pub reason: Option<String>,
    pub is_breakpoint: bool,
    pub breakpoint_type: Option<String>,
    pub duration_ms: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
}

impl ApprovalAudit {
    fn from_row(row: &Row<'_>) -> Result<Self, StoreError> {
        Ok(Self {
            id: row.get("id")?,
            conversation_id: row.get("conversation_id")?,
            run_id: row.get("run_id")?,
            call_id: row.get("call_id")?,
            tool_name: row.get("tool_name")?,
            arguments: parse_json(row.get("arguments")?)?,
            approved: row.get::<_, i64>("approved")? != 0,
            decision_source: row.get("decision_source")?,
            decided_by: row.get("decided_by")?,
            reason: row.get("reason")?,
            is_breakpoint: row.get::<_, i64>("is_breakpoint")? != 0,
            breakpoint_type: row.get("breakpoint_type")?,
            duration_ms: row.get("duration_ms")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewApprovalAudit {
    pub run_id: Option<i64>,
    pub call_id: String,
    pub tool_name: String,
    pub arguments: Option<Value>,
    pub approved: bool,
    pub decision_source: String,
    pub decided_by: Option<String>,
    pub reason: Option<String>,
    pub is_breakpoint: bool,
    pub breakpoint_type: Option<String>,
    pub duration_ms: Option<i64>,
}

impl crate::LegacyStore {
    pub fn record_approval_audit(
        &self,
        actor_id: &str,
        conversation_id: i64,
        new: &NewApprovalAudit,
    ) -> Result<ApprovalAudit, StoreError> {
        let connection = self.connection()?;
        require_conversation(&connection, actor_id, conversation_id)?;
        let timestamp = now_python();
        connection.execute(
            "INSERT INTO approval_audits(created_at, updated_at, conversation_id, run_id, call_id,
               tool_name, arguments, approved, decision_source, decided_by, reason, is_breakpoint,
               breakpoint_type, duration_ms)
             VALUES (?1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                timestamp,
                conversation_id,
                new.run_id,
                new.call_id,
                new.tool_name,
                json_text(&new.arguments)?,
                i64::from(new.approved),
                new.decision_source,
                new.decided_by,
                new.reason,
                i64::from(new.is_breakpoint),
                new.breakpoint_type,
                new.duration_ms,
            ],
        )?;
        let id = connection.last_insert_rowid();
        query_one(
            &connection,
            "SELECT * FROM approval_audits WHERE id = ?1",
            [id],
            ApprovalAudit::from_row,
        )?
        .ok_or(StoreError::NotFound("approval audit"))
    }

    pub fn list_approval_audits(
        &self,
        actor_id: &str,
        conversation_id: i64,
        run_id: Option<i64>,
        limit: Option<usize>,
    ) -> Result<Vec<ApprovalAudit>, StoreError> {
        let connection = self.connection()?;
        require_conversation(&connection, actor_id, conversation_id)?;
        let mut statement = connection.prepare(
            "SELECT * FROM approval_audits WHERE conversation_id = ?1 \
             AND (?2 IS NULL OR run_id = ?2) ORDER BY id DESC LIMIT ?3",
        )?;
        let rows = statement.query(params![
            conversation_id,
            run_id,
            bounded_limit(limit, 100, 1_000),
        ])?;
        collect_rows(rows, ApprovalAudit::from_row)
    }
}

/// Read the legacy `users.id` of an existing run (diagnostics/tests).
pub fn run_owner(connection: &Connection, run_id: i64) -> Result<Option<i64>, StoreError> {
    Ok(connection
        .query_row(
            "SELECT user_id FROM agent_runs WHERE id = ?1",
            [run_id],
            |row| row.get(0),
        )
        .optional()?)
}
