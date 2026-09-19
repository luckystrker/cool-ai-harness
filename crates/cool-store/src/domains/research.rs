//! Legacy `research_runs` store module (M10).
//!
//! Mirrors `backend/app/research/orchestrator.py` lifecycle semantics: a run is
//! created, then finished (completed/failed/cancelled) or cancelled. The
//! `input_hash` groups reruns with identical inputs.

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::domains::common::{
    bounded_limit, collect_rows, json_text, parse_json, query_one, require_conversation,
    user_id_for,
};
use crate::error::StoreError;
use crate::time::now_python;

/// Allowed research statuses (`RESEARCH_STATUSES`).
pub const RESEARCH_STATUSES: &[&str] = &["queued", "running", "completed", "failed", "cancelled"];
/// Terminal research statuses.
pub const TERMINAL_RESEARCH_STATUSES: &[&str] = &["completed", "failed", "cancelled"];
/// Decomposition depth bounds (`RESEARCH_DEPTH_MIN`/`MAX`).
pub const RESEARCH_DEPTH_MIN: i64 = 3;
pub const RESEARCH_DEPTH_MAX: i64 = 5;
/// Default decomposition depth (`RESEARCH_DEPTH_DEFAULT`).
pub const RESEARCH_DEPTH_DEFAULT: i64 = 4;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResearchRun {
    pub id: i64,
    pub user_id: i64,
    pub conversation_id: Option<i64>,
    pub parent_task_run_id: Option<i64>,
    pub topic: String,
    pub depth: i64,
    pub model: Option<String>,
    pub status: String,
    pub sub_questions: Option<Value>,
    pub sources: Option<Value>,
    pub citations: Option<Value>,
    pub report_markdown: Option<String>,
    pub report_artifact_id: Option<i64>,
    pub usage: Option<Value>,
    pub error: Option<String>,
    pub input_hash: Option<String>,
    pub finished_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl ResearchRun {
    fn from_row(row: &Row<'_>) -> Result<Self, StoreError> {
        Ok(Self {
            id: row.get("id")?,
            user_id: row.get("user_id")?,
            conversation_id: row.get("conversation_id")?,
            parent_task_run_id: row.get("parent_task_run_id")?,
            topic: row.get::<_, Option<String>>("topic")?.unwrap_or_default(),
            depth: row.get("depth")?,
            model: row.get("model")?,
            status: row.get("status")?,
            sub_questions: parse_json(row.get("sub_questions")?)?,
            sources: parse_json(row.get("sources")?)?,
            citations: parse_json(row.get("citations")?)?,
            report_markdown: row.get("report_markdown")?,
            report_artifact_id: row.get("report_artifact_id")?,
            usage: parse_json(row.get("usage")?)?,
            error: row.get("error")?,
            input_hash: row.get("input_hash")?,
            finished_at: row.get("finished_at")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewResearchRun {
    pub topic: String,
    pub depth: i64,
    pub model: Option<String>,
    pub conversation_id: Option<i64>,
    pub parent_task_run_id: Option<i64>,
}

impl Default for NewResearchRun {
    fn default() -> Self {
        Self {
            topic: String::new(),
            depth: RESEARCH_DEPTH_DEFAULT,
            model: None,
            conversation_id: None,
            parent_task_run_id: None,
        }
    }
}

fn input_hash(topic: &str, depth: i64, model: Option<&str>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(format!("{topic}|{depth}|{}", model.unwrap_or("")).as_bytes());
    format!("{:x}", hasher.finalize())
}

fn fetch_research_run(
    connection: &Connection,
    user_id: i64,
    run_id: i64,
) -> Result<Option<ResearchRun>, StoreError> {
    query_one(
        connection,
        "SELECT * FROM research_runs WHERE id = ?1 AND user_id = ?2",
        params![run_id, user_id],
        ResearchRun::from_row,
    )
}

impl crate::LegacyStore {
    /// Research runs for the actor, newest first.
    pub fn list_research_runs(
        &self,
        actor_id: &str,
        limit: Option<usize>,
    ) -> Result<Vec<ResearchRun>, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        let mut statement = connection
            .prepare("SELECT * FROM research_runs WHERE user_id = ?1 ORDER BY id DESC LIMIT ?2")?;
        let rows = statement.query(params![user_id, bounded_limit(limit, 50, 200)])?;
        collect_rows(rows, ResearchRun::from_row)
    }

    pub fn get_research_run(&self, actor_id: &str, run_id: i64) -> Result<ResearchRun, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        fetch_research_run(&connection, user_id, run_id)?
            .ok_or(StoreError::NotFound("research run"))
    }

    /// Create a research run in the `running` state.
    // NEEDS(PY): `start_research` persists `queued` (then flips to running);
    // the M10 store contract requires a newly created run to read `running`.
    pub fn create_research_run(
        &self,
        actor_id: &str,
        new: &NewResearchRun,
    ) -> Result<ResearchRun, StoreError> {
        if !(RESEARCH_DEPTH_MIN..=RESEARCH_DEPTH_MAX).contains(&new.depth) {
            return Err(StoreError::InvalidInput(format!(
                "depth must be between {RESEARCH_DEPTH_MIN} and {RESEARCH_DEPTH_MAX}"
            )));
        }
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        if let Some(conversation_id) = new.conversation_id {
            require_conversation(&connection, actor_id, conversation_id)?;
        }
        let timestamp = now_python();
        let hash = input_hash(&new.topic, new.depth, new.model.as_deref());
        connection.execute(
            "INSERT INTO research_runs(created_at, updated_at, user_id, conversation_id,
               parent_task_run_id, topic, depth, model, status, input_hash)
             VALUES (?1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, 'running', ?8)",
            params![
                timestamp,
                user_id,
                new.conversation_id,
                new.parent_task_run_id,
                new.topic,
                new.depth,
                new.model,
                hash,
            ],
        )?;
        let id = connection.last_insert_rowid();
        drop(connection);
        self.get_research_run(actor_id, id)
    }

    /// Finish a run with a terminal status, updating `finished_at`.
    #[allow(clippy::too_many_arguments)]
    pub fn finish_research_run(
        &self,
        actor_id: &str,
        run_id: i64,
        status: &str,
        report_markdown: Option<&str>,
        sources: Option<&Value>,
        citations: Option<&Value>,
        sub_questions: Option<&Value>,
        usage: Option<&Value>,
        error: Option<&str>,
        report_artifact_id: Option<i64>,
    ) -> Result<ResearchRun, StoreError> {
        if !TERMINAL_RESEARCH_STATUSES.contains(&status) {
            return Err(StoreError::InvalidInput(format!(
                "research status {status:?} is not terminal"
            )));
        }
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        fetch_research_run(&connection, user_id, run_id)?
            .ok_or(StoreError::NotFound("research run"))?;
        if let Some(artifact_id) = report_artifact_id {
            let conversation: Option<i64> = connection
                .query_row(
                    "SELECT conversation_id FROM artifacts WHERE id = ?1",
                    [artifact_id],
                    |row| row.get(0),
                )
                .optional()?;
            match conversation {
                Some(conversation_id) => {
                    require_conversation(&connection, actor_id, conversation_id)?;
                }
                None => return Err(StoreError::NotFound("artifact")),
            }
        }
        let timestamp = now_python();
        connection.execute(
            "UPDATE research_runs SET status = ?1, report_markdown = COALESCE(?2, report_markdown),
               sources = COALESCE(?3, sources), citations = COALESCE(?4, citations),
               sub_questions = COALESCE(?5, sub_questions), usage = COALESCE(?6, usage),
               error = COALESCE(?7, error),
               report_artifact_id = COALESCE(?8, report_artifact_id),
               finished_at = ?9, updated_at = ?9 WHERE id = ?10",
            params![
                status,
                report_markdown,
                json_text(&sources.cloned())?,
                json_text(&citations.cloned())?,
                json_text(&sub_questions.cloned())?,
                json_text(&usage.cloned())?,
                error,
                report_artifact_id,
                timestamp,
                run_id,
            ],
        )?;
        drop(connection);
        self.get_research_run(actor_id, run_id)
    }

    /// Cancel a non-terminal run.
    pub fn cancel_research_run(
        &self,
        actor_id: &str,
        run_id: i64,
    ) -> Result<ResearchRun, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        let run = fetch_research_run(&connection, user_id, run_id)?
            .ok_or(StoreError::NotFound("research run"))?;
        if TERMINAL_RESEARCH_STATUSES.contains(&run.status.as_str()) {
            return Err(StoreError::NotFound("research run"));
        }
        let timestamp = now_python();
        connection.execute(
            "UPDATE research_runs SET status = 'cancelled', finished_at = ?1, updated_at = ?1
             WHERE id = ?2",
            params![timestamp, run_id],
        )?;
        drop(connection);
        self.get_research_run(actor_id, run_id)
    }
}
