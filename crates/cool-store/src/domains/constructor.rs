//! Persistent user-defined macro tools (legacy `macro_tools` table, Phase 4
//! Agent Constructor).
//!
//! Macro tools are actor-scoped through `macro_tools.user_id`. Tool-registry
//! validation and live registration from `app/agent/constructor.py` belong to
//! the runtime, not the store; only persistence and the `name` conflict rule
//! are implemented here.

use rusqlite::{Connection, Row, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::domains::common::{collect_rows, parse_json, query_one, user_id_for};
use crate::error::StoreError;
use crate::time::now_python;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MacroTool {
    pub id: i64,
    pub user_id: i64,
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub steps: Value,
    pub is_active: bool,
    pub created_at: String,
    pub updated_at: String,
}

impl MacroTool {
    fn from_row(row: &Row<'_>) -> Result<Self, StoreError> {
        Ok(Self {
            id: row.get("id")?,
            user_id: row.get("user_id")?,
            name: row.get("name")?,
            description: row.get("description")?,
            input_schema: parse_json(row.get("input_schema")?)?
                .unwrap_or_else(|| json!({"type": "object", "properties": {}})),
            steps: parse_json(row.get("steps")?)?.unwrap_or(Value::Array(Vec::new())),
            is_active: row.get::<_, i64>("is_active")? != 0,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

/// Macro creation payload. `is_active` defaults to `true` (model default).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct NewMacroTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub steps: Value,
    pub is_active: bool,
}

impl Default for NewMacroTool {
    fn default() -> Self {
        Self {
            name: String::new(),
            description: String::new(),
            input_schema: json!({"type": "object", "properties": {}}),
            steps: Value::Array(Vec::new()),
            is_active: true,
        }
    }
}

/// Partial update. `name` is immutable, matching `constructor.update_macro`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MacroToolPatch {
    pub description: Option<String>,
    pub input_schema: Option<Value>,
    pub steps: Option<Value>,
    pub is_active: Option<bool>,
}

fn fetch_macro_tool(
    connection: &Connection,
    user_id: i64,
    macro_id: i64,
) -> Result<MacroTool, StoreError> {
    let macro_tool = query_one(
        connection,
        "SELECT * FROM macro_tools WHERE id = ?1",
        [macro_id],
        MacroTool::from_row,
    )?
    .ok_or(StoreError::NotFound("macro tool"))?;
    if macro_tool.user_id != user_id {
        return Err(StoreError::NotFound("macro tool"));
    }
    Ok(macro_tool)
}

impl crate::LegacyStore {
    /// Macros owned by the actor, ordered by name.
    pub fn list_macro_tools(
        &self,
        actor_id: &str,
        include_inactive: bool,
    ) -> Result<Vec<MacroTool>, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        let mut statement = connection.prepare(
            "SELECT * FROM macro_tools WHERE user_id = ?1 AND (?2 = 1 OR is_active = 1) \
             ORDER BY name ASC, id ASC",
        )?;
        let rows = statement.query(params![user_id, i64::from(include_inactive)])?;
        collect_rows(rows, MacroTool::from_row)
    }

    pub fn get_macro_tool(&self, actor_id: &str, macro_id: i64) -> Result<MacroTool, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        fetch_macro_tool(&connection, user_id, macro_id)
    }

    /// Create a macro. `name` is globally unique → [`StoreError::Conflict`].
    pub fn create_macro_tool(
        &self,
        actor_id: &str,
        new: &NewMacroTool,
    ) -> Result<MacroTool, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        let existing: Option<i64> = query_one(
            &connection,
            "SELECT id FROM macro_tools WHERE name = ?1",
            [new.name.as_str()],
            |row| Ok(row.get(0)?),
        )?;
        if existing.is_some() {
            return Err(StoreError::Conflict(format!(
                "macro tool '{}' already exists",
                new.name
            )));
        }
        let timestamp = now_python();
        connection.execute(
            "INSERT INTO macro_tools(created_at, updated_at, user_id, name, description,
               input_schema, steps, is_active)
             VALUES (?1, ?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                timestamp,
                user_id,
                new.name,
                new.description,
                serde_json::to_string(&new.input_schema)?,
                serde_json::to_string(&new.steps)?,
                i64::from(new.is_active),
            ],
        )?;
        let id = connection.last_insert_rowid();
        drop(connection);
        self.get_macro_tool(actor_id, id)
    }

    pub fn update_macro_tool(
        &self,
        actor_id: &str,
        macro_id: i64,
        patch: &MacroToolPatch,
    ) -> Result<MacroTool, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        fetch_macro_tool(&connection, user_id, macro_id)?;
        let mut assignments: Vec<&str> = Vec::new();
        let mut values: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(description) = &patch.description {
            assignments.push("description = ?");
            values.push(Box::new(description.clone()));
        }
        if let Some(schema) = &patch.input_schema {
            assignments.push("input_schema = ?");
            values.push(Box::new(serde_json::to_string(schema)?));
        }
        if let Some(steps) = &patch.steps {
            assignments.push("steps = ?");
            values.push(Box::new(serde_json::to_string(steps)?));
        }
        if let Some(active) = patch.is_active {
            assignments.push("is_active = ?");
            values.push(Box::new(i64::from(active)));
        }
        if !assignments.is_empty() {
            assignments.push("updated_at = ?");
            values.push(Box::new(now_python()));
            values.push(Box::new(macro_id));
            let sql = format!(
                "UPDATE macro_tools SET {} WHERE id = ?",
                assignments.join(", ")
            );
            let references: Vec<&dyn rusqlite::ToSql> =
                values.iter().map(|value| value.as_ref()).collect();
            connection.execute(&sql, references.as_slice())?;
        }
        drop(connection);
        self.get_macro_tool(actor_id, macro_id)
    }

    pub fn delete_macro_tool(&self, actor_id: &str, macro_id: i64) -> Result<(), StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        fetch_macro_tool(&connection, user_id, macro_id)?;
        connection.execute("DELETE FROM macro_tools WHERE id = ?1", [macro_id])?;
        Ok(())
    }
}
