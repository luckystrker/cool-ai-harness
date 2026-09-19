//! Plans, plan steps and plan templates (legacy `plans`, `plan_steps`,
//! `plan_templates` tables, Фаза 2 §1).
//!
//! Plans belong to a conversation, so every plan read/write validates actor
//! ownership through [`common::require_conversation`]. `plan_steps` and
//! `plan_templates` have no owner column; their accessors mirror the Python
//! service and are documented as globally scoped.

use rusqlite::{Connection, Row, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::domains::common::{collect_rows, parse_json, query_one, require_conversation};
use crate::error::StoreError;
use crate::time::now_python;

pub const PLAN_STATUSES: &[&str] = &[
    "draft",
    "approved",
    "executing",
    "completed",
    "failed",
    "cancelled",
];

pub const PLAN_STEP_STATUSES: &[&str] = &["pending", "running", "completed", "failed", "skipped"];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    pub id: i64,
    pub conversation_id: i64,
    pub run_id: Option<i64>,
    pub title: Option<String>,
    pub status: String,
    pub steps: Option<Value>,
    pub metadata: Option<Value>,
    pub created_at: String,
    pub updated_at: String,
}

impl Plan {
    fn from_row(row: &Row<'_>) -> Result<Self, StoreError> {
        Ok(Self {
            id: row.get("id")?,
            conversation_id: row.get("conversation_id")?,
            run_id: row.get("run_id")?,
            title: row.get("title")?,
            status: row.get("status")?,
            steps: parse_json(row.get("steps")?)?,
            metadata: parse_json(row.get("metadata_")?)?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanStep {
    pub id: i64,
    pub plan_id: i64,
    pub position: i64,
    pub title: String,
    pub description: Option<String>,
    pub status: String,
    pub depends_on: Option<Value>,
    pub tools: Option<Value>,
    pub result_summary: Option<String>,
    pub run_id: Option<i64>,
    pub delegate_role: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl PlanStep {
    fn from_row(row: &Row<'_>) -> Result<Self, StoreError> {
        Ok(Self {
            id: row.get("id")?,
            plan_id: row.get("plan_id")?,
            position: row.get("position")?,
            title: row.get("title")?,
            description: row.get("description")?,
            status: row.get("status")?,
            depends_on: parse_json(row.get("depends_on")?)?,
            tools: parse_json(row.get("tools")?)?,
            result_summary: row.get("result_summary")?,
            run_id: row.get("run_id")?,
            delegate_role: row.get("delegate_role")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanTemplate {
    pub id: i64,
    pub name: String,
    pub description: Option<String>,
    pub steps: Value,
    pub is_builtin: bool,
    pub created_at: String,
    pub updated_at: String,
}

impl PlanTemplate {
    fn from_row(row: &Row<'_>) -> Result<Self, StoreError> {
        Ok(Self {
            id: row.get("id")?,
            name: row.get("name")?,
            description: row.get("description")?,
            steps: parse_json(row.get("steps")?)?.unwrap_or(Value::Array(Vec::new())),
            is_builtin: row.get::<_, i64>("is_builtin")? != 0,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

fn fetch_plan(
    connection: &Connection,
    conversation_id: i64,
    plan_id: i64,
) -> Result<Plan, StoreError> {
    query_one(
        connection,
        "SELECT * FROM plans WHERE id = ?1 AND conversation_id = ?2",
        params![plan_id, conversation_id],
        Plan::from_row,
    )?
    .ok_or(StoreError::NotFound("plan"))
}

/// Serialize an optional JSON fragment, treating explicit `null` as `NULL`.
fn json_fragment(value: Option<&Value>) -> Result<Option<String>, StoreError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(value) => Ok(Some(serde_json::to_string(value)?)),
    }
}

/// Rebuild `plan_steps` rows for a plan from a JSON array. An explicit
/// `position` is preserved; otherwise the array index is used (0..n).
fn insert_plan_steps(
    connection: &Connection,
    plan_id: i64,
    steps: &[Value],
    timestamp: &str,
) -> Result<(), StoreError> {
    for (index, step) in steps.iter().enumerate() {
        let position = step
            .get("position")
            .and_then(Value::as_i64)
            .unwrap_or(index as i64);
        let title = step
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("Untitled");
        let description = step.get("description").and_then(Value::as_str);
        let depends_on = json_fragment(step.get("depends_on"))?;
        let tools = json_fragment(step.get("tools"))?;
        let delegate_role = step.get("delegate_role").and_then(Value::as_str);
        connection.execute(
            "INSERT INTO plan_steps(created_at, updated_at, plan_id, position, title, description,
               status, depends_on, tools, result_summary, run_id, delegate_role)
             VALUES (?1, ?1, ?2, ?3, ?4, ?5, 'pending', ?6, ?7, NULL, NULL, ?8)",
            params![
                timestamp,
                plan_id,
                position,
                title,
                description,
                depends_on,
                tools,
                delegate_role,
            ],
        )?;
    }
    Ok(())
}

impl crate::LegacyStore {
    /// Plans for a conversation, newest first.
    pub fn list_plans(
        &self,
        actor_id: &str,
        conversation_id: i64,
    ) -> Result<Vec<Plan>, StoreError> {
        let connection = self.connection()?;
        require_conversation(&connection, actor_id, conversation_id)?;
        let mut statement = connection
            .prepare("SELECT * FROM plans WHERE conversation_id = ?1 ORDER BY id DESC")?;
        let rows = statement.query([conversation_id])?;
        collect_rows(rows, Plan::from_row)
    }

    pub fn get_plan(
        &self,
        actor_id: &str,
        conversation_id: i64,
        plan_id: i64,
    ) -> Result<Plan, StoreError> {
        let connection = self.connection()?;
        require_conversation(&connection, actor_id, conversation_id)?;
        fetch_plan(&connection, conversation_id, plan_id)
    }

    /// Persist a draft plan and its step rows. `steps` must be a JSON array.
    pub fn create_plan(
        &self,
        actor_id: &str,
        conversation_id: i64,
        run_id: Option<i64>,
        title: Option<&str>,
        steps: &Value,
    ) -> Result<Plan, StoreError> {
        let steps_array = steps.as_array().ok_or_else(|| {
            StoreError::InvalidInput("plan steps must be a JSON array".to_string())
        })?;
        let mut connection = self.connection()?;
        require_conversation(&connection, actor_id, conversation_id)?;
        let timestamp = now_python();
        let transaction = connection.transaction()?;
        transaction.execute(
            "INSERT INTO plans(created_at, updated_at, conversation_id, run_id, title, status,
               steps, metadata_)
             VALUES (?1, ?1, ?2, ?3, ?4, 'draft', ?5, NULL)",
            params![
                timestamp,
                conversation_id,
                run_id,
                title,
                serde_json::to_string(steps)?,
            ],
        )?;
        let plan_id = transaction.last_insert_rowid();
        insert_plan_steps(&transaction, plan_id, steps_array, &timestamp)?;
        transaction.commit()?;
        drop(connection);
        self.get_plan(actor_id, conversation_id, plan_id)
    }

    /// Replace all step rows of a draft plan from a JSON array, matching
    /// `planning.update_plan_steps` (draft-only).
    pub fn replace_plan_steps(
        &self,
        actor_id: &str,
        conversation_id: i64,
        plan_id: i64,
        steps: &Value,
    ) -> Result<Plan, StoreError> {
        let steps_array = steps.as_array().ok_or_else(|| {
            StoreError::InvalidInput("plan steps must be a JSON array".to_string())
        })?;
        let mut connection = self.connection()?;
        require_conversation(&connection, actor_id, conversation_id)?;
        let plan = fetch_plan(&connection, conversation_id, plan_id)?;
        if plan.status != "draft" {
            return Err(StoreError::InvalidInput(format!(
                "plan {plan_id} is not in draft status"
            )));
        }
        let timestamp = now_python();
        let transaction = connection.transaction()?;
        transaction.execute("DELETE FROM plan_steps WHERE plan_id = ?1", [plan_id])?;
        insert_plan_steps(&transaction, plan_id, steps_array, &timestamp)?;
        transaction.execute(
            "UPDATE plans SET steps = ?1, updated_at = ?2 WHERE id = ?3",
            params![serde_json::to_string(steps)?, timestamp, plan_id],
        )?;
        transaction.commit()?;
        drop(connection);
        self.get_plan(actor_id, conversation_id, plan_id)
    }

    /// Set a plan status, validating the Python plan-status vocabulary.
    pub fn set_plan_status(
        &self,
        actor_id: &str,
        conversation_id: i64,
        plan_id: i64,
        status: &str,
    ) -> Result<Plan, StoreError> {
        if !PLAN_STATUSES.contains(&status) {
            return Err(StoreError::InvalidInput(format!(
                "unknown plan status {status:?}"
            )));
        }
        let connection = self.connection()?;
        require_conversation(&connection, actor_id, conversation_id)?;
        fetch_plan(&connection, conversation_id, plan_id)?;
        connection.execute(
            "UPDATE plans SET status = ?1, updated_at = ?2 WHERE id = ?3",
            params![status, now_python(), plan_id],
        )?;
        drop(connection);
        self.get_plan(actor_id, conversation_id, plan_id)
    }

    /// Delete a plan and its step rows.
    ///
    /// Python exposes no delete endpoint for plans; this durable-store helper
    /// keeps the child rows consistent when a conversation is torn down.
    pub fn delete_plan(
        &self,
        actor_id: &str,
        conversation_id: i64,
        plan_id: i64,
    ) -> Result<(), StoreError> {
        let mut connection = self.connection()?;
        require_conversation(&connection, actor_id, conversation_id)?;
        fetch_plan(&connection, conversation_id, plan_id)?;
        let transaction = connection.transaction()?;
        transaction.execute("DELETE FROM plan_steps WHERE plan_id = ?1", [plan_id])?;
        transaction.execute("DELETE FROM plans WHERE id = ?1", [plan_id])?;
        transaction.commit()?;
        Ok(())
    }

    /// Steps of a plan ordered by position.
    ///
    /// Globally readable by plan id: `plan_steps` has no owner column and the
    /// Python service (`planning.get_plan_steps`) is unscoped.
    pub fn list_plan_steps(&self, plan_id: i64) -> Result<Vec<PlanStep>, StoreError> {
        let connection = self.connection()?;
        let mut statement = connection
            .prepare("SELECT * FROM plan_steps WHERE plan_id = ?1 ORDER BY position ASC, id ASC")?;
        let rows = statement.query([plan_id])?;
        collect_rows(rows, PlanStep::from_row)
    }

    /// Update a single step (addressed by plan id + position), validating the
    /// step-status vocabulary.
    pub fn update_plan_step_status(
        &self,
        plan_id: i64,
        position: i64,
        status: &str,
        result_summary: Option<&str>,
        run_id: Option<i64>,
    ) -> Result<PlanStep, StoreError> {
        if !PLAN_STEP_STATUSES.contains(&status) {
            return Err(StoreError::InvalidInput(format!(
                "unknown plan step status {status:?}"
            )));
        }
        let connection = self.connection()?;
        let updated = connection.execute(
            "UPDATE plan_steps SET status = ?1, result_summary = ?2, run_id = ?3, updated_at = ?4
             WHERE plan_id = ?5 AND position = ?6",
            params![
                status,
                result_summary,
                run_id,
                now_python(),
                plan_id,
                position,
            ],
        )?;
        if updated == 0 {
            return Err(StoreError::NotFound("plan step"));
        }
        query_one(
            &connection,
            "SELECT * FROM plan_steps WHERE plan_id = ?1 AND position = ?2",
            params![plan_id, position],
            PlanStep::from_row,
        )?
        .ok_or(StoreError::NotFound("plan step"))
    }

    /// Plan templates, ordered by id (Python `list_templates`).
    pub fn list_plan_templates(&self) -> Result<Vec<PlanTemplate>, StoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare("SELECT * FROM plan_templates ORDER BY id ASC")?;
        let rows = statement.query([])?;
        collect_rows(rows, PlanTemplate::from_row)
    }

    pub fn create_plan_template(
        &self,
        name: &str,
        description: Option<&str>,
        steps: &Value,
    ) -> Result<PlanTemplate, StoreError> {
        if !steps.is_array() {
            return Err(StoreError::InvalidInput(
                "plan template steps must be a JSON array".to_string(),
            ));
        }
        let connection = self.connection()?;
        let timestamp = now_python();
        connection.execute(
            "INSERT INTO plan_templates(created_at, updated_at, name, description, steps, is_builtin)
             VALUES (?1, ?1, ?2, ?3, ?4, 0)",
            params![timestamp, name, description, serde_json::to_string(steps)?],
        )?;
        let id = connection.last_insert_rowid();
        query_one(
            &connection,
            "SELECT * FROM plan_templates WHERE id = ?1",
            [id],
            PlanTemplate::from_row,
        )?
        .ok_or(StoreError::NotFound("plan template"))
    }

    /// Delete a non-built-in template (Python `delete_template`).
    pub fn delete_plan_template(&self, template_id: i64) -> Result<(), StoreError> {
        let connection = self.connection()?;
        let template = query_one(
            &connection,
            "SELECT * FROM plan_templates WHERE id = ?1",
            [template_id],
            PlanTemplate::from_row,
        )?
        .ok_or(StoreError::NotFound("plan template"))?;
        if template.is_builtin {
            return Err(StoreError::InvalidInput(
                "Cannot delete a built-in plan template".to_string(),
            ));
        }
        connection.execute("DELETE FROM plan_templates WHERE id = ?1", [template_id])?;
        Ok(())
    }
}
