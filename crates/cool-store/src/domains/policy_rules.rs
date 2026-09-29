//! Persisted `policy_rules` rows (user-scoped policy rules, P1.6).
//!
//! Rows are tool-call rules evaluated before the capability fallback in the
//! agent loop. `scope` is normally `user`; `project` rows carry a
//! `project_key` so several workspaces can coexist in one adopted database —
//! the app-server also persists project rules to `<workspace>/.cool/policy.json`,
//! which takes precedence for its own workspace.

use rusqlite::{Row, params};
use serde::{Deserialize, Serialize};

use crate::domains::common::collect_rows;
use crate::error::StoreError;
use crate::time::now_python;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicyRuleRow {
    pub id: i64,
    /// Tool name, or `"*"` for every tool.
    pub tool: String,
    /// `command` | `path_glob` | `domain` | `any`.
    pub pattern_kind: String,
    pub pattern: String,
    /// `allow` | `ask` | `deny`.
    pub decision: String,
    /// `user` | `project`.
    pub scope: String,
    pub project_key: Option<String>,
    pub note: Option<String>,
    pub created_at: String,
    pub created_by: Option<String>,
}

impl PolicyRuleRow {
    fn from_row(row: &Row<'_>) -> Result<Self, StoreError> {
        Ok(Self {
            id: row.get("id")?,
            tool: row.get("tool")?,
            pattern_kind: row.get("pattern_kind")?,
            pattern: row.get("pattern")?,
            decision: row.get("decision")?,
            scope: row.get("scope")?,
            project_key: row.get("project_key")?,
            note: row.get("note")?,
            created_at: row.get("created_at")?,
            created_by: row.get("created_by")?,
        })
    }
}

/// Fields required to insert a rule.
#[derive(Clone, Debug, PartialEq)]
pub struct NewPolicyRule {
    pub tool: String,
    pub pattern_kind: String,
    pub pattern: String,
    pub decision: String,
    pub scope: String,
    pub project_key: Option<String>,
    pub note: Option<String>,
    pub created_by: Option<String>,
}

impl crate::LegacyStore {
    /// All user-scoped rules (and any `project` rows for `project_key` when
    /// supplied), ordered by id — first match wins upstream.
    pub fn list_policy_rules(
        &self,
        project_key: Option<&str>,
    ) -> Result<Vec<PolicyRuleRow>, StoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT * FROM policy_rules \
             WHERE scope = 'user' OR (scope = 'project' AND project_key = ?1) \
             ORDER BY id ASC",
        )?;
        let rows = statement.query(params![project_key])?;
        collect_rows(rows, PolicyRuleRow::from_row)
    }

    pub fn insert_policy_rule(&self, new: &NewPolicyRule) -> Result<PolicyRuleRow, StoreError> {
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO policy_rules(\
               tool, pattern_kind, pattern, decision, scope, project_key, note, created_at, created_by\
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                new.tool,
                new.pattern_kind,
                new.pattern,
                new.decision,
                new.scope,
                new.project_key,
                new.note,
                now_python(),
                new.created_by,
            ],
        )?;
        let id = connection.last_insert_rowid();
        let mut statement = connection.prepare("SELECT * FROM policy_rules WHERE id = ?1")?;
        let mut rows = statement.query(params![id])?;
        let row = rows.next()?.ok_or(StoreError::NotFound("policy rule"))?;
        PolicyRuleRow::from_row(row)
    }

    pub fn delete_policy_rule(&self, rule_id: i64) -> Result<bool, StoreError> {
        let connection = self.connection()?;
        let deleted = connection.execute("DELETE FROM policy_rules WHERE id = ?1", [rule_id])?;
        Ok(deleted > 0)
    }
}
