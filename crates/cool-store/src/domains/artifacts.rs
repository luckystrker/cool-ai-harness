//! Legacy `artifacts` store module (M10).
//!
//! Mirrors `backend/app/artifacts/__init__.py`: rows are owned through their
//! conversation, soft-deletes keep the blob, and a child artifact inherits
//! `version = parent.version + 1`.

use rusqlite::{Connection, Row, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::domains::common::{
    bounded_limit, collect_rows, json_text, parse_json, query_one, require_conversation,
};
use crate::error::StoreError;
use crate::time::now_python;

/// Coarse artifact classification (mirrors `ARTIFACT_KINDS`).
pub const ARTIFACT_KINDS: &[&str] = &[
    "file",
    "image",
    "document",
    "code",
    "report",
    "audio",
    "tool_result",
];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Artifact {
    pub id: i64,
    pub conversation_id: i64,
    pub run_id: Option<i64>,
    pub tool_call_id: Option<String>,
    pub filename: String,
    pub media_type: String,
    pub kind: String,
    pub size_bytes: i64,
    pub sha256: Option<String>,
    pub storage_path: String,
    pub version: i64,
    pub parent_id: Option<i64>,
    pub metadata: Option<Value>,
    pub extracted_text: Option<String>,
    pub is_deleted: bool,
    pub created_at: String,
    pub updated_at: String,
}

impl Artifact {
    fn from_row(row: &Row<'_>) -> Result<Self, StoreError> {
        Ok(Self {
            id: row.get("id")?,
            conversation_id: row.get("conversation_id")?,
            run_id: row.get("run_id")?,
            tool_call_id: row.get("tool_call_id")?,
            filename: row.get("filename")?,
            media_type: row.get("media_type")?,
            kind: row.get("kind")?,
            size_bytes: row.get("size_bytes")?,
            sha256: row.get("sha256")?,
            storage_path: row.get("storage_path")?,
            version: row.get("version")?,
            parent_id: row.get("parent_id")?,
            metadata: parse_json(row.get("metadata_")?)?,
            extracted_text: row.get("extracted_text")?,
            is_deleted: row.get::<_, i64>("is_deleted")? != 0,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewArtifact {
    pub filename: String,
    pub media_type: String,
    pub kind: String,
    pub size_bytes: i64,
    pub sha256: Option<String>,
    pub storage_path: String,
    pub tool_call_id: Option<String>,
    pub parent_id: Option<i64>,
    pub metadata: Option<Value>,
    pub run_id: Option<i64>,
}

/// Fetch an artifact row verbatim, including soft-deleted ones.
fn fetch_artifact(
    connection: &Connection,
    artifact_id: i64,
) -> Result<Option<Artifact>, StoreError> {
    query_one(
        connection,
        "SELECT * FROM artifacts WHERE id = ?1",
        [artifact_id],
        Artifact::from_row,
    )
}

impl crate::LegacyStore {
    /// Artifacts for a conversation, newest first.
    pub fn list_artifacts(
        &self,
        actor_id: &str,
        conversation_id: i64,
        include_deleted: bool,
        limit: Option<usize>,
    ) -> Result<Vec<Artifact>, StoreError> {
        let connection = self.connection()?;
        require_conversation(&connection, actor_id, conversation_id)?;
        let mut statement = connection.prepare(
            "SELECT * FROM artifacts WHERE conversation_id = ?1 \
             AND (?2 = 1 OR is_deleted = 0) ORDER BY id DESC LIMIT ?3",
        )?;
        let rows = statement.query(params![
            conversation_id,
            i64::from(include_deleted),
            bounded_limit(limit, 100, 500),
        ])?;
        collect_rows(rows, Artifact::from_row)
    }

    /// Artifacts for a conversation with optional run/kind filters.
    pub fn list_artifacts_filtered(
        &self,
        actor_id: &str,
        conversation_id: i64,
        run_id: Option<i64>,
        kind: Option<&str>,
        include_deleted: bool,
        limit: Option<usize>,
    ) -> Result<Vec<Artifact>, StoreError> {
        let connection = self.connection()?;
        require_conversation(&connection, actor_id, conversation_id)?;
        let mut statement = connection.prepare(
            "SELECT * FROM artifacts WHERE conversation_id = ?1 \
             AND (?2 = 1 OR is_deleted = 0) \
             AND (?3 IS NULL OR run_id = ?3) \
             AND (?4 IS NULL OR kind = ?4) ORDER BY id DESC LIMIT ?5",
        )?;
        let rows = statement.query(params![
            conversation_id,
            i64::from(include_deleted),
            run_id,
            kind,
            bounded_limit(limit, 100, 500),
        ])?;
        collect_rows(rows, Artifact::from_row)
    }

    /// Artifact metadata; soft-deleted artifacts read as not found (Python).
    pub fn get_artifact(&self, actor_id: &str, artifact_id: i64) -> Result<Artifact, StoreError> {
        let connection = self.connection()?;
        let artifact =
            fetch_artifact(&connection, artifact_id)?.ok_or(StoreError::NotFound("artifact"))?;
        require_conversation(&connection, actor_id, artifact.conversation_id)?;
        if artifact.is_deleted {
            return Err(StoreError::NotFound("artifact"));
        }
        Ok(artifact)
    }

    /// Register metadata for an artifact whose blob was already stored.
    pub fn register_artifact(
        &self,
        actor_id: &str,
        conversation_id: i64,
        new: &NewArtifact,
    ) -> Result<Artifact, StoreError> {
        let connection = self.connection()?;
        require_conversation(&connection, actor_id, conversation_id)?;
        let version = match new.parent_id {
            Some(parent_id) => match fetch_artifact(&connection, parent_id)? {
                Some(parent) => {
                    require_conversation(&connection, actor_id, parent.conversation_id)?;
                    parent.version + 1
                }
                None => 1,
            },
            None => 1,
        };
        let timestamp = now_python();
        connection.execute(
            "INSERT INTO artifacts(created_at, updated_at, conversation_id, run_id, tool_call_id,
               filename, media_type, kind, size_bytes, sha256, storage_path, version, parent_id,
               metadata_, extracted_text, is_deleted)
             VALUES (?1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, NULL, 0)",
            params![
                timestamp,
                conversation_id,
                new.run_id,
                new.tool_call_id,
                new.filename,
                new.media_type,
                new.kind,
                new.size_bytes,
                new.sha256,
                new.storage_path,
                version,
                new.parent_id,
                json_text(&new.metadata)?,
            ],
        )?;
        let id = connection.last_insert_rowid();
        drop(connection);
        self.get_artifact(actor_id, id)
    }

    /// Soft-delete an artifact (blob stays on disk).
    pub fn soft_delete_artifact(&self, actor_id: &str, artifact_id: i64) -> Result<(), StoreError> {
        let connection = self.connection()?;
        let artifact =
            fetch_artifact(&connection, artifact_id)?.ok_or(StoreError::NotFound("artifact"))?;
        require_conversation(&connection, actor_id, artifact.conversation_id)?;
        if artifact.is_deleted {
            return Err(StoreError::NotFound("artifact"));
        }
        connection.execute(
            "UPDATE artifacts SET is_deleted = 1, updated_at = ?1 WHERE id = ?2",
            params![now_python(), artifact_id],
        )?;
        Ok(())
    }

    /// Newest live artifact with the given content hash, if any.
    pub fn find_artifact_by_sha256(
        &self,
        actor_id: &str,
        conversation_id: i64,
        sha256: &str,
    ) -> Result<Option<Artifact>, StoreError> {
        let connection = self.connection()?;
        require_conversation(&connection, actor_id, conversation_id)?;
        query_one(
            &connection,
            "SELECT * FROM artifacts WHERE conversation_id = ?1 AND sha256 = ?2 \
             AND is_deleted = 0 ORDER BY id DESC LIMIT 1",
            params![conversation_id, sha256],
            Artifact::from_row,
        )
    }

    /// Persist extracted text for an artifact.
    pub fn set_artifact_extracted_text(
        &self,
        actor_id: &str,
        artifact_id: i64,
        text: &str,
    ) -> Result<(), StoreError> {
        let connection = self.connection()?;
        let artifact =
            fetch_artifact(&connection, artifact_id)?.ok_or(StoreError::NotFound("artifact"))?;
        require_conversation(&connection, actor_id, artifact.conversation_id)?;
        connection.execute(
            "UPDATE artifacts SET extracted_text = ?1, updated_at = ?2 WHERE id = ?3",
            params![text, now_python(), artifact_id],
        )?;
        Ok(())
    }
}
