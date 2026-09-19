//! Subagent roles and runs (legacy `subagent_roles` / `subagent_runs` tables,
//! Фаза 2 §5).
//!
//! Roles are **globally scoped** (`subagent_roles` has no `user_id`), matching
//! `app/agent/subagents.py`. Runs are owned through their parent conversation:
//! every read/write validates `require_conversation` against
//! `parent_conversation_id`.

use rusqlite::{Connection, Row, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::domains::common::{
    bounded_limit, collect_rows, json_text, parse_json, query_one, require_conversation,
    user_id_for,
};
use crate::error::StoreError;
use crate::time::now_python;

pub const SUBAGENT_STATUSES: &[&str] = &["queued", "running", "completed", "failed", "cancelled"];

pub const TERMINAL_SUBAGENT_STATUSES: &[&str] = &["completed", "failed", "cancelled"];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentRole {
    pub id: i64,
    pub name: String,
    pub description: Option<String>,
    pub system_prompt: Option<String>,
    pub model: Option<String>,
    pub tool_names: Option<Value>,
    pub capability_policy: Option<Value>,
    pub max_iterations: i64,
    pub max_cost_usd: Option<f64>,
    pub is_builtin: bool,
    pub created_at: String,
    pub updated_at: String,
}

impl SubagentRole {
    fn from_row(row: &Row<'_>) -> Result<Self, StoreError> {
        Ok(Self {
            id: row.get("id")?,
            name: row.get("name")?,
            description: row.get("description")?,
            system_prompt: row.get("system_prompt")?,
            model: row.get("model")?,
            tool_names: parse_json(row.get("tool_names")?)?,
            capability_policy: parse_json(row.get("capability_policy")?)?,
            max_iterations: row.get("max_iterations")?,
            max_cost_usd: row.get("max_cost_usd")?,
            is_builtin: row.get::<_, i64>("is_builtin")? != 0,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

/// Role creation payload. `max_iterations` defaults to 10 and `is_builtin` to
/// `false`, matching `subagents.create_role`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct NewSubagentRole {
    pub name: String,
    pub description: Option<String>,
    pub system_prompt: Option<String>,
    pub model: Option<String>,
    pub tool_names: Option<Value>,
    pub capability_policy: Option<Value>,
    pub max_iterations: i64,
    pub max_cost_usd: Option<f64>,
    pub is_builtin: bool,
}

impl Default for NewSubagentRole {
    fn default() -> Self {
        Self {
            name: String::new(),
            description: None,
            system_prompt: None,
            model: None,
            tool_names: None,
            capability_policy: None,
            max_iterations: 10,
            max_cost_usd: None,
            is_builtin: false,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentRolePatch {
    pub name: Option<String>,
    pub description: Option<String>,
    pub system_prompt: Option<String>,
    /// Empty string clears the model.
    pub model: Option<String>,
    pub tool_names: Option<Value>,
    pub capability_policy: Option<Value>,
    pub max_iterations: Option<i64>,
    /// `Some(None)` clears the cost limit; `Some(Some(v))` sets it.
    pub max_cost_usd: Option<Option<f64>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentRun {
    pub id: i64,
    pub role_id: Option<i64>,
    pub profile_id: Option<i64>,
    pub parent_conversation_id: i64,
    pub parent_run_id: Option<i64>,
    pub research_run_id: Option<i64>,
    pub conversation_id: i64,
    pub run_id: Option<i64>,
    pub name: Option<String>,
    pub prompt: String,
    pub status: String,
    pub result_summary: Option<String>,
    pub usage: Option<Value>,
    pub error: Option<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl SubagentRun {
    fn from_row(row: &Row<'_>) -> Result<Self, StoreError> {
        Ok(Self {
            id: row.get("id")?,
            role_id: row.get("role_id")?,
            profile_id: row.get("profile_id")?,
            parent_conversation_id: row.get("parent_conversation_id")?,
            parent_run_id: row.get("parent_run_id")?,
            research_run_id: row.get("research_run_id")?,
            conversation_id: row.get("conversation_id")?,
            run_id: row.get("run_id")?,
            name: row.get("name")?,
            prompt: row.get("prompt")?,
            status: row.get("status")?,
            result_summary: row.get("result_summary")?,
            usage: parse_json(row.get("usage")?)?,
            error: row.get("error")?,
            started_at: row.get("started_at")?,
            finished_at: row.get("finished_at")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

/// Fields required to create a subagent run. The isolated conversation is
/// supplied by the caller (Python creates it inside `create_subagent_run`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewSubagentRun {
    pub role_id: Option<i64>,
    pub parent_run_id: Option<i64>,
    pub conversation_id: i64,
    pub name: Option<String>,
    pub prompt: String,
    pub profile_id: Option<i64>,
    pub research_run_id: Option<i64>,
}

#[derive(Clone, Debug, Default)]
pub struct SubagentRunFilter {
    pub parent_conversation_id: Option<i64>,
    pub status: Option<String>,
    pub research_run_id: Option<i64>,
    pub limit: Option<usize>,
}

fn fetch_role(connection: &Connection, role_id: i64) -> Result<SubagentRole, StoreError> {
    query_one(
        connection,
        "SELECT * FROM subagent_roles WHERE id = ?1",
        [role_id],
        SubagentRole::from_row,
    )?
    .ok_or(StoreError::NotFound("subagent role"))
}

/// Fetch a run and validate ownership of its parent conversation.
fn fetch_run(
    connection: &Connection,
    actor_id: &str,
    run_id: i64,
) -> Result<SubagentRun, StoreError> {
    let run = query_one(
        connection,
        "SELECT * FROM subagent_runs WHERE id = ?1",
        [run_id],
        SubagentRun::from_row,
    )?
    .ok_or(StoreError::NotFound("subagent run"))?;
    require_conversation(connection, actor_id, run.parent_conversation_id)?;
    Ok(run)
}

impl crate::LegacyStore {
    /// Subagent roles ordered by name. Globally scoped (no owner column).
    pub fn list_subagent_roles(&self) -> Result<Vec<SubagentRole>, StoreError> {
        let connection = self.connection()?;
        let mut statement =
            connection.prepare("SELECT * FROM subagent_roles ORDER BY name ASC, id ASC")?;
        let rows = statement.query([])?;
        collect_rows(rows, SubagentRole::from_row)
    }

    pub fn get_subagent_role(&self, role_id: i64) -> Result<SubagentRole, StoreError> {
        let connection = self.connection()?;
        fetch_role(&connection, role_id)
    }

    pub fn create_subagent_role(&self, new: &NewSubagentRole) -> Result<SubagentRole, StoreError> {
        let connection = self.connection()?;
        let timestamp = now_python();
        connection.execute(
            "INSERT INTO subagent_roles(created_at, updated_at, name, description, system_prompt,
               model, tool_names, capability_policy, max_iterations, max_cost_usd, is_builtin)
             VALUES (?1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                timestamp,
                new.name,
                new.description,
                new.system_prompt,
                new.model,
                json_text(&new.tool_names)?,
                json_text(&new.capability_policy)?,
                new.max_iterations,
                new.max_cost_usd,
                i64::from(new.is_builtin),
            ],
        )?;
        let id = connection.last_insert_rowid();
        drop(connection);
        self.get_subagent_role(id)
    }

    pub fn update_subagent_role(
        &self,
        role_id: i64,
        patch: &SubagentRolePatch,
    ) -> Result<SubagentRole, StoreError> {
        let connection = self.connection()?;
        fetch_role(&connection, role_id)?;
        let mut assignments: Vec<&str> = Vec::new();
        let mut values: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(name) = &patch.name {
            assignments.push("name = ?");
            values.push(Box::new(name.clone()));
        }
        if let Some(description) = &patch.description {
            assignments.push("description = ?");
            values.push(Box::new(description.clone()));
        }
        if let Some(prompt) = &patch.system_prompt {
            assignments.push("system_prompt = ?");
            values.push(Box::new(prompt.clone()));
        }
        if let Some(model) = &patch.model {
            assignments.push("model = ?");
            values.push(Box::new(if model.is_empty() {
                None
            } else {
                Some(model.clone())
            }));
        }
        if patch.tool_names.is_some() {
            assignments.push("tool_names = ?");
            values.push(Box::new(json_text(&patch.tool_names)?));
        }
        if patch.capability_policy.is_some() {
            assignments.push("capability_policy = ?");
            values.push(Box::new(json_text(&patch.capability_policy)?));
        }
        if let Some(iterations) = patch.max_iterations {
            assignments.push("max_iterations = ?");
            values.push(Box::new(iterations));
        }
        if let Some(cost) = patch.max_cost_usd {
            assignments.push("max_cost_usd = ?");
            values.push(Box::new(cost));
        }
        if !assignments.is_empty() {
            assignments.push("updated_at = ?");
            values.push(Box::new(now_python()));
            values.push(Box::new(role_id));
            let sql = format!(
                "UPDATE subagent_roles SET {} WHERE id = ?",
                assignments.join(", ")
            );
            let references: Vec<&dyn rusqlite::ToSql> =
                values.iter().map(|value| value.as_ref()).collect();
            connection.execute(&sql, references.as_slice())?;
        }
        drop(connection);
        self.get_subagent_role(role_id)
    }

    /// Delete a non-built-in role (Python `delete_role`).
    pub fn delete_subagent_role(&self, role_id: i64) -> Result<(), StoreError> {
        let connection = self.connection()?;
        let role = fetch_role(&connection, role_id)?;
        if role.is_builtin {
            return Err(StoreError::InvalidInput(
                "Cannot delete a built-in role".to_string(),
            ));
        }
        connection.execute("DELETE FROM subagent_roles WHERE id = ?1", [role_id])?;
        Ok(())
    }

    /// Create a queued subagent run bound to an already-created isolated
    /// conversation. Validates ownership of the parent conversation.
    pub fn create_subagent_run(
        &self,
        actor_id: &str,
        parent_conversation_id: i64,
        new: &NewSubagentRun,
    ) -> Result<SubagentRun, StoreError> {
        let connection = self.connection()?;
        require_conversation(&connection, actor_id, parent_conversation_id)?;
        let timestamp = now_python();
        connection.execute(
            "INSERT INTO subagent_runs(created_at, updated_at, role_id, profile_id,
               parent_conversation_id, parent_run_id, research_run_id, conversation_id, run_id,
               name, prompt, status, started_at)
             VALUES (?1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, ?8, ?9, 'queued', ?1)",
            params![
                timestamp,
                new.role_id,
                new.profile_id,
                parent_conversation_id,
                new.parent_run_id,
                new.research_run_id,
                new.conversation_id,
                new.name,
                new.prompt,
            ],
        )?;
        let id = connection.last_insert_rowid();
        drop(connection);
        self.get_subagent_run(actor_id, id)
    }

    /// Subagent runs owned by the actor's parent conversations, newest first.
    pub fn list_subagent_runs(
        &self,
        actor_id: &str,
        filter: &SubagentRunFilter,
    ) -> Result<Vec<SubagentRun>, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        if let Some(parent_conversation_id) = filter.parent_conversation_id {
            require_conversation(&connection, actor_id, parent_conversation_id)?;
        }
        let mut statement = connection.prepare(
            "SELECT * FROM subagent_runs \
             WHERE parent_conversation_id IN (SELECT id FROM conversations WHERE user_id = ?1) \
             AND (?2 IS NULL OR parent_conversation_id = ?2) \
             AND (?3 IS NULL OR status = ?3) \
             AND (?4 IS NULL OR research_run_id = ?4) \
             ORDER BY id DESC LIMIT ?5",
        )?;
        let rows = statement.query(params![
            user_id,
            filter.parent_conversation_id,
            filter.status,
            filter.research_run_id,
            bounded_limit(filter.limit, 50, 500),
        ])?;
        collect_rows(rows, SubagentRun::from_row)
    }

    pub fn get_subagent_run(&self, actor_id: &str, run_id: i64) -> Result<SubagentRun, StoreError> {
        let connection = self.connection()?;
        fetch_run(&connection, actor_id, run_id)
    }

    /// Mark a run terminal (`completed` / `failed` / `cancelled`) and stamp
    /// `finished_at`.
    pub fn finish_subagent_run(
        &self,
        actor_id: &str,
        run_id: i64,
        status: &str,
        result_summary: Option<&str>,
        usage: Option<&Value>,
        error: Option<&str>,
    ) -> Result<SubagentRun, StoreError> {
        if !TERMINAL_SUBAGENT_STATUSES.contains(&status) {
            return Err(StoreError::InvalidInput(format!(
                "subagent status {status:?} is not terminal"
            )));
        }
        let connection = self.connection()?;
        fetch_run(&connection, actor_id, run_id)?;
        let timestamp = now_python();
        connection.execute(
            "UPDATE subagent_runs SET status = ?1, result_summary = COALESCE(?2, result_summary),
               usage = COALESCE(?3, usage), error = COALESCE(?4, error), finished_at = ?5,
               updated_at = ?5 WHERE id = ?6",
            params![
                status,
                result_summary,
                usage.map(serde_json::to_string).transpose()?,
                error,
                timestamp,
                run_id,
            ],
        )?;
        drop(connection);
        self.get_subagent_run(actor_id, run_id)
    }

    /// Delete a terminal subagent run (Python `delete_subagent_run`).
    pub fn delete_subagent_run(&self, actor_id: &str, run_id: i64) -> Result<(), StoreError> {
        let connection = self.connection()?;
        let run = fetch_run(&connection, actor_id, run_id)?;
        if !TERMINAL_SUBAGENT_STATUSES.contains(&run.status.as_str()) {
            return Err(StoreError::InvalidInput(
                "Cannot delete a non-terminal subagent run".to_string(),
            ));
        }
        connection.execute("DELETE FROM subagent_runs WHERE id = ?1", [run_id])?;
        Ok(())
    }
}
