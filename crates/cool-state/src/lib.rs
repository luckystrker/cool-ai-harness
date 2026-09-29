//! M6 durable state owned by the Rust trusted core.
//!
//! The tables are deliberately namespaced with `rust_`.  M6 proves the new
//! state/security semantics without dual-writing or taking ownership of the
//! Python application's existing schema; that cutover belongs to M10.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use cool_protocol::{
    ActorKind, ActorRef, ApprovalDecision, ApprovalOutcome, CanonicalEvent, EventEnvelope,
    Extensions, ItemEvent, RunCancelledResult, RunStarted, RunTerminal, SteerAcceptedResult,
    ToolApprovalRequired, ToolApprovalResolved, ToolFailed, V1Version, WorkerEvent,
};
use cool_security::mask_json;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use uuid::Uuid;

const SCHEMA_VERSION: i64 = 3;

#[derive(Debug)]
pub enum StoreError {
    Sqlite(rusqlite::Error),
    Json(serde_json::Error),
    Io(std::io::Error),
    InvalidTransition {
        from: RunStatus,
        to: RunStatus,
    },
    IdempotencyConflict,
    NotFound(&'static str),
    ActorMismatch,
    RevisionConflict,
    AlreadyResolved,
    RunNotActive,
    BudgetExceeded(BudgetSnapshot),
    /// `session.rewind` rejected the target state (live run or nothing to
    /// rewind). The message is a stable machine-readable reason.
    RewindRejected(&'static str),
    Corrupt(String),
}

impl fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sqlite(error) => write!(formatter, "SQLite error: {error}"),
            Self::Json(error) => write!(formatter, "JSON error: {error}"),
            Self::Io(error) => write!(formatter, "I/O error: {error}"),
            Self::InvalidTransition { from, to } => {
                write!(formatter, "invalid run transition {from:?} -> {to:?}")
            }
            Self::IdempotencyConflict => formatter.write_str("idempotency key conflict"),
            Self::NotFound(kind) => write!(formatter, "{kind} not found"),
            Self::ActorMismatch => formatter.write_str("actor does not own this record"),
            Self::RevisionConflict => formatter.write_str("revision conflict"),
            Self::AlreadyResolved => formatter.write_str("approval is already resolved"),
            Self::RunNotActive => formatter.write_str("run is not active"),
            Self::RewindRejected(reason) => write!(formatter, "rewind rejected: {reason}"),
            Self::BudgetExceeded(snapshot) => write!(
                formatter,
                "budget exceeded at {} tokens / {} micro-USD",
                snapshot.tokens, snapshot.cost_microusd
            ),
            Self::Corrupt(message) => write!(formatter, "durable state is corrupt: {message}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<rusqlite::Error> for StoreError {
    fn from(value: rusqlite::Error) -> Self {
        Self::Sqlite(value)
    }
}

impl From<serde_json::Error> for StoreError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

impl From<std::io::Error> for StoreError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

#[derive(Clone)]
pub struct DurableStore {
    connection: Arc<Mutex<Connection>>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Queued,
    Running,
    AwaitingApproval,
    Completed,
    Failed,
    Cancelled,
    /// The run was superseded by a `session.rewind`: its events stay durable
    /// in the append-only log but leave the session's visible history.
    Rewound,
}

impl RunStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Cancelled | Self::Rewound
        )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::AwaitingApproval => "awaiting_approval",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Rewound => "rewound",
        }
    }

    fn parse(value: &str) -> Result<Self, StoreError> {
        match value {
            "queued" => Ok(Self::Queued),
            "running" => Ok(Self::Running),
            "awaiting_approval" => Ok(Self::AwaitingApproval),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            "rewound" => Ok(Self::Rewound),
            other => Err(StoreError::Corrupt(format!("unknown run status {other}"))),
        }
    }
}

fn transition_allowed(from: RunStatus, to: RunStatus) -> bool {
    from == to
        || matches!(
            (from, to),
            (RunStatus::Queued, RunStatus::Running)
                | (RunStatus::Queued, RunStatus::Cancelled)
                | (RunStatus::Running, RunStatus::AwaitingApproval)
                | (RunStatus::Running, RunStatus::Completed)
                | (RunStatus::Running, RunStatus::Failed)
                | (RunStatus::Running, RunStatus::Cancelled)
                | (RunStatus::AwaitingApproval, RunStatus::Running)
                | (RunStatus::AwaitingApproval, RunStatus::Failed)
                | (RunStatus::AwaitingApproval, RunStatus::Cancelled)
        )
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SessionListEntry {
    pub session_id: String,
    pub title: Option<String>,
    pub project_key: Option<String>,
    pub active_run_id: Option<String>,
    pub last_seq: Option<u64>,
    pub created_at: String,
}

/// What `session.rewind` left behind: the seed run, the superseded runs and
/// the newest filesystem checkpoint ref found in the retained history (P2.18).
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SessionRewindOutcome {
    pub run_id: String,
    pub rewound_run_ids: Vec<String>,
    pub checkpoint_ref: Option<String>,
    /// Workspace paths the discarded file tools named (`write_file`/
    /// `edit_file` `path` args) — the set a scoped `git clean` removes on
    /// restore so unrelated untracked files survive. Shell/git mutations
    /// carry no path and stay non-restorable as documented.
    #[serde(default)]
    pub discarded_paths: Vec<String>,
    /// Every `checkpoint_ref` found beyond the cursor, in order —
    /// `checkpoint_ref` is the first entry. Manifest checkpoints cover only
    /// the file their own call touched, so a manifest-backend restore must
    /// replay all of them newest-first; a git tree snapshot already covers
    /// the whole workspace and restores once.
    #[serde(default)]
    pub checkpoint_refs: Vec<String>,
}

/// Result of binding a legacy conversation to a durable Rust session.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ConversationLink {
    pub conversation_id: i64,
    pub session_id: String,
    pub created: bool,
    pub imported_events: u64,
    pub truncated: bool,
}

/// One projected legacy transcript item imported into a session.
#[derive(Clone, Debug, PartialEq)]
pub struct ImportedHistoryEvent {
    pub occurred_at: String,
    pub event: CanonicalEvent,
}

/// One durable run row of a session, newest-first when listed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionRunEntry {
    pub run_id: String,
    pub status: RunStatus,
    pub last_seq: u64,
    pub finish_reason: Option<String>,
    pub updated_at: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionSnapshot {
    pub session_id: String,
    pub actor_id: String,
    pub title: Option<String>,
    pub project_key: Option<String>,
    pub active_run_id: Option<String>,
    pub last_seq: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunSnapshot {
    pub run_id: String,
    pub session_id: String,
    pub actor_id: String,
    pub status: RunStatus,
    pub last_seq: u64,
    pub checkpoint: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IdempotentOutcome<T> {
    pub value: T,
    pub created: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CancelAcceptance {
    pub result: RunCancelledResult,
    pub created: bool,
    pub events: Vec<EventEnvelope>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EventProvenance {
    pub actor: ActorRef,
    pub source: String,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BudgetLimits {
    pub tokens: Option<u64>,
    pub cost_microusd: Option<u64>,
    pub iterations: Option<u64>,
    pub proactive_actions: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BudgetDelta {
    pub tokens: u64,
    pub cost_microusd: Option<u64>,
    pub iterations: u64,
    pub proactive_actions: u64,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BudgetSnapshot {
    pub tokens: u64,
    pub cost_microusd: u64,
    pub iterations: u64,
    pub proactive_actions: u64,
    pub revision: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApprovalTicket {
    pub approval_id: String,
    pub revision: u64,
    pub created: bool,
}

/// Where an approval ticket points before it is resolved.
#[derive(Clone, Debug, PartialEq)]
pub struct ApprovalCallContext {
    pub session_id: String,
    pub run_id: String,
    pub call_id: String,
    /// `pending` / `approved` / `denied`.
    pub state: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ApprovalResolution {
    pub approval_id: String,
    pub run_id: String,
    pub session_id: String,
    pub call_id: String,
    pub revision: u64,
    pub outcome: ApprovalOutcome,
    /// Question-ask payload resolved alongside the decision (`ask_user`,
    /// `breakpointType: "question"`); `None` for plain allow/deny approvals.
    pub answer: Option<serde_json::Value>,
    pub created: bool,
    pub event: EventEnvelope,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactReference {
    pub artifact_id: String,
    pub session_id: String,
    pub run_id: Option<String>,
    pub sha256: String,
    pub size_bytes: u64,
    pub storage_path: String,
    pub actor_id: String,
    pub source: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerStatus {
    Starting,
    Running,
    Failed,
    Stopped,
}

impl WorkerStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Failed => "failed",
            Self::Stopped => "stopped",
        }
    }
}

impl DurableStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        Self::from_connection(Connection::open(path)?)
    }

    pub fn in_memory() -> Result<Self, StoreError> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(connection: Connection) -> Result<Self, StoreError> {
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        connection.execute_batch("PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL;")?;
        migrate(&connection)?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    fn connection(&self) -> Result<MutexGuard<'_, Connection>, StoreError> {
        self.connection
            .lock()
            .map_err(|_| StoreError::Corrupt("connection mutex poisoned".to_owned()))
    }

    pub fn create_session(
        &self,
        actor_id: &str,
        key: &str,
        fingerprint: &str,
        title: Option<&str>,
        project_key: Option<&str>,
    ) -> Result<IdempotentOutcome<String>, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(existing) = lookup_idempotency::<String>(
            &transaction,
            actor_id,
            "session.create",
            key,
            fingerprint,
        )? {
            transaction.commit()?;
            return Ok(IdempotentOutcome {
                value: existing,
                created: false,
            });
        }
        let session_id = format!("session-{}", Uuid::new_v4());
        transaction.execute(
            "INSERT INTO rust_sessions(id, actor_id, title, project_key, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![session_id, actor_id, title, project_key, timestamp()],
        )?;
        insert_idempotency(
            &transaction,
            actor_id,
            "session.create",
            key,
            fingerprint,
            &session_id,
        )?;
        transaction.commit()?;
        Ok(IdempotentOutcome {
            value: session_id,
            created: true,
        })
    }

    pub fn load_session(
        &self,
        session_id: &str,
        actor_id: &str,
    ) -> Result<SessionSnapshot, StoreError> {
        let connection = self.connection()?;
        let snapshot = connection
            .query_row(
                "SELECT s.actor_id, s.title, s.project_key, s.active_run_id, MAX(e.seq) \
                 FROM rust_sessions s LEFT JOIN rust_events e ON e.run_id = s.active_run_id \
                 WHERE s.id = ?1 GROUP BY s.id",
                [session_id],
                |row| {
                    Ok(SessionSnapshot {
                        session_id: session_id.to_owned(),
                        actor_id: row.get(0)?,
                        title: row.get(1)?,
                        project_key: row.get(2)?,
                        active_run_id: row.get(3)?,
                        last_seq: row.get::<_, Option<i64>>(4)?.map(|value| value as u64),
                    })
                },
            )
            .optional()?
            .ok_or(StoreError::NotFound("session"))?;
        if snapshot.actor_id != actor_id {
            return Err(StoreError::ActorMismatch);
        }
        Ok(snapshot)
    }

    pub fn session_events(
        &self,
        session_id: &str,
        actor_id: &str,
    ) -> Result<Vec<EventEnvelope>, StoreError> {
        let connection = self.connection()?;
        let owner = connection
            .query_row(
                "SELECT actor_id FROM rust_sessions WHERE id = ?1",
                [session_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or(StoreError::NotFound("session"))?;
        if owner != actor_id {
            return Err(StoreError::ActorMismatch);
        }
        let mut statement = connection.prepare(
            "SELECT e.envelope_json FROM rust_runs r JOIN rust_events e ON e.run_id = r.id \
             WHERE r.session_id = ?1 ORDER BY r.rowid, e.seq",
        )?;
        let rows = statement.query_map([session_id], |row| row.get::<_, String>(0))?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }

    /// Newest-first window of a session's canonical events, paginated on the
    /// append-only `rust_events.rowid` cursor. `before_cursor` is exclusive;
    /// `limit + 1` rows are read so the caller can report `has_more`. The
    /// returned rows are oldest-first and carry their cursor.
    pub fn session_event_window(
        &self,
        session_id: &str,
        actor_id: &str,
        before_cursor: Option<u64>,
        limit: usize,
    ) -> Result<Vec<(u64, EventEnvelope)>, StoreError> {
        let connection = self.connection()?;
        let owner = connection
            .query_row(
                "SELECT actor_id FROM rust_sessions WHERE id = ?1",
                [session_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or(StoreError::NotFound("session"))?;
        if owner != actor_id {
            return Err(StoreError::ActorMismatch);
        }
        if before_cursor.is_some_and(|value| value > i64::MAX as u64) {
            return Ok(Vec::new());
        }
        // Runs superseded by a `session.rewind` keep their durable events
        // but leave the session's visible history window.
        let mut statement = connection.prepare(
            "SELECT e.rowid, e.envelope_json FROM rust_runs r JOIN rust_events e ON e.run_id = r.id \
             WHERE r.session_id = ?1 AND r.status != 'rewound' \
             AND (?2 IS NULL OR e.rowid < ?2) \
             ORDER BY e.rowid DESC LIMIT ?3",
        )?;
        let rows = statement.query_map(
            params![
                session_id,
                before_cursor.map(|value| value as i64),
                limit as i64
            ],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )?;
        let mut window = rows
            .map(|row| {
                let (cursor, json) = row?;
                Ok::<_, StoreError>((cursor as u64, serde_json::from_str(&json)?))
            })
            .collect::<Result<Vec<_>, _>>()?;
        window.reverse();
        Ok(window)
    }

    pub fn list_sessions(
        &self,
        actor_id: &str,
        project_key: Option<&str>,
        limit: usize,
    ) -> Result<Vec<SessionListEntry>, StoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT s.id, s.title, s.project_key, s.active_run_id, MAX(e.seq), s.created_at \
             FROM rust_sessions s LEFT JOIN rust_events e ON e.run_id = s.active_run_id \
             WHERE s.actor_id = ?1 AND (?2 IS NULL OR s.project_key = ?2) \
             GROUP BY s.id ORDER BY s.rowid DESC LIMIT ?3",
        )?;
        let rows = statement.query_map(params![actor_id, project_key, limit as i64], |row| {
            Ok(SessionListEntry {
                session_id: row.get(0)?,
                title: row.get(1)?,
                project_key: row.get(2)?,
                active_run_id: row.get(3)?,
                last_seq: row.get::<_, Option<i64>>(4)?.map(|value| value as u64),
                created_at: row.get(5)?,
            })
        })?;
        rows.map(|row| Ok(row?)).collect()
    }

    /// Fork a session, optionally bounded to a history prefix.
    ///
    /// `up_to_cursor` bounds on the durable event cursor (`HistoryItem.cursor`
    /// = `rust_events.rowid`); `up_to_event_seq` bounds on each event's own
    /// `seq`. Both bounds apply when both are set. Events of runs superseded
    /// by a rewind (`rewound` status) are never copied.
    #[allow(clippy::too_many_arguments)]
    pub fn fork_session(
        &self,
        actor_id: &str,
        key: &str,
        fingerprint: &str,
        source_session_id: &str,
        title: Option<&str>,
        up_to_cursor: Option<u64>,
        up_to_event_seq: Option<u64>,
    ) -> Result<IdempotentOutcome<String>, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(existing) =
            lookup_idempotency::<String>(&transaction, actor_id, "session.fork", key, fingerprint)?
        {
            transaction.commit()?;
            return Ok(IdempotentOutcome {
                value: existing,
                created: false,
            });
        }
        let (owner, source_title, project_key): (String, Option<String>, Option<String>) =
            transaction
                .query_row(
                    "SELECT actor_id, title, project_key FROM rust_sessions WHERE id = ?1",
                    [source_session_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?
                .ok_or(StoreError::NotFound("session"))?;
        if owner != actor_id {
            return Err(StoreError::ActorMismatch);
        }
        let session_id = format!("session-{}", Uuid::new_v4());
        let run_id = format!("run-{}", Uuid::new_v4());
        let now = timestamp();
        transaction.execute(
            "INSERT INTO rust_sessions(id, actor_id, title, project_key, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                session_id,
                actor_id,
                title.or(source_title.as_deref()),
                project_key,
                now
            ],
        )?;
        transaction.execute(
            "INSERT INTO rust_runs(id, session_id, actor_id, status, last_seq, updated_at) VALUES (?1, ?2, ?3, 'running', 0, ?4)",
            params![run_id, session_id, actor_id, now],
        )?;
        transaction.execute(
            "UPDATE rust_sessions SET active_run_id = ?1 WHERE id = ?2",
            params![run_id, session_id],
        )?;

        let mut next_seq = 0_u64;
        let mut fork_events = vec![EventEnvelope {
            event_id: format!("event-{}", Uuid::new_v4()),
            schema_version: V1Version::VALUE,
            session_id: session_id.clone(),
            run_id: run_id.clone(),
            item_id: None,
            seq: 0,
            occurred_at: now.clone(),
            actor: ActorRef {
                id: "cool-core".to_owned(),
                kind: ActorKind::System,
            },
            source: "cool-state-fork".to_owned(),
            causation_id: None,
            correlation_id: None,
            event: CanonicalEvent::RunStarted(RunStarted {
                model: None,
                mode: Some("fork".to_owned()),
            }),
            extensions: Default::default(),
        }];
        let mut copied = Vec::new();
        {
            // `seq` is per run, so the merged history must keep the durable
            // (run rowid, seq) order and must never be re-sorted by seq alone.
            // `up_to_cursor` bounds the event rowid — the same cursor space
            // `session.history` exposes — while `up_to_event_seq` bounds seq.
            let mut statement = transaction.prepare(
                "SELECT e.rowid, e.envelope_json FROM rust_runs r JOIN rust_events e ON e.run_id = r.id \
                 WHERE r.session_id = ?1 AND r.status != 'rewound' \
                 AND (?2 IS NULL OR e.rowid <= ?2) AND (?3 IS NULL OR e.seq <= ?3) \
                 ORDER BY r.rowid, e.seq",
            )?;
            let rows = statement.query_map(
                params![
                    source_session_id,
                    up_to_cursor.map(|value| value.min(i64::MAX as u64) as i64),
                    up_to_event_seq.map(|value| value.min(i64::MAX as u64) as i64)
                ],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )?;
            for row in rows {
                let (source_rowid, json) = row?;
                let mut source: EventEnvelope = serde_json::from_str(&json)?;
                if !is_history_event(&source.event) {
                    continue;
                }
                // Workspace-bound extensions (`checkpoint_ref` — the
                // P2.18 key, kept as a literal since cool-state can't see
                // cool-agent's constant) name refs in the source session's
                // working tree; on the fork they either don't resolve or
                // would restore the wrong tree — strip them.
                source.extensions.remove("checkpoint_ref");
                copied.push((source_rowid, source));
            }
        }
        for (_, source) in &copied {
            fork_events.push(EventEnvelope {
                event_id: format!("event-{}", Uuid::new_v4()),
                schema_version: V1Version::VALUE,
                session_id: session_id.clone(),
                run_id: run_id.clone(),
                item_id: source.item_id.clone(),
                seq: 0,
                occurred_at: source.occurred_at.clone(),
                actor: source.actor.clone(),
                source: "cool-state-fork".to_owned(),
                causation_id: Some(source.event_id.clone()),
                correlation_id: source.correlation_id.clone(),
                event: source.event.clone(),
                extensions: source.extensions.clone(),
            });
        }
        fork_events.push(EventEnvelope {
            event_id: format!("event-{}", Uuid::new_v4()),
            schema_version: V1Version::VALUE,
            session_id: session_id.clone(),
            run_id: run_id.clone(),
            item_id: None,
            seq: 0,
            occurred_at: timestamp(),
            actor: ActorRef {
                id: "cool-core".to_owned(),
                kind: ActorKind::System,
            },
            source: "cool-state-fork".to_owned(),
            causation_id: None,
            correlation_id: None,
            event: CanonicalEvent::RunCompleted(RunTerminal {
                reason: "fork".to_owned(),
                error_code: None,
            }),
            extensions: Default::default(),
        });
        // Copied `session.compacted` markers carry `compact_up_to_cursor`
        // in the SOURCE rowid space; remap it to the rowid the covering
        // prefix got here or the transcript re-shows compacted messages.
        let mut boundary_rowid = 0_i64;
        let mut inserted_rowids: Vec<(i64, i64)> = Vec::new();
        for (index, event) in fork_events.iter_mut().enumerate() {
            next_seq += 1;
            event.seq = next_seq;
            if index > 0
                && let CanonicalEvent::SessionCompacted(compacted) = &mut event.event
                && let Some(covered) = compacted.compact_up_to_cursor
            {
                let covered = covered.min(i64::MAX as u64) as i64;
                compacted.compact_up_to_cursor = Some(
                    inserted_rowids
                        .iter()
                        .rev()
                        .find(|(source_rowid, _)| *source_rowid <= covered)
                        .map(|(_, new_rowid)| *new_rowid)
                        .unwrap_or(boundary_rowid)
                        .max(0) as u64,
                );
            }
            append_event_tx(&transaction, actor_id, event)?;
            let new_rowid = transaction.last_insert_rowid();
            if index == 0 {
                boundary_rowid = new_rowid;
            } else if index <= copied.len() {
                inserted_rowids.push((copied[index - 1].0, new_rowid));
            }
        }
        insert_idempotency(
            &transaction,
            actor_id,
            "session.fork",
            key,
            fingerprint,
            &session_id,
        )?;
        transaction.commit()?;
        Ok(IdempotentOutcome {
            value: session_id,
            created: true,
        })
    }

    /// Rewind a session to `to_cursor`, append-only.
    ///
    /// Every existing run is superseded — marked `rewound`, a terminal status
    /// that removes its events from the session's visible history while the
    /// append-only log keeps them durable — and a fresh seed run records the
    /// retained history prefix: `RunStarted(mode="rewind")`, copies of the
    /// history events at or below the cursor, a `run.rewound` marker and
    /// `RunCompleted(reason="rewind")`. The session ends with no active run,
    /// so the next prompt builds history from exactly the retained prefix.
    ///
    /// Rejects with `RewindRejected` when any run is still live or nothing
    /// sits beyond the cursor. The returned `checkpoint_ref` is the
    /// pre-dispatch snapshot of the FIRST mutating call beyond the cursor —
    /// the workspace state the retained prefix ended in (P2.18).
    ///
    /// Only events in runs that are not already `rewound` feed the retained
    /// prefix or the checkpoint scan, so a second rewind can neither copy
    /// previously discarded history nor select a superseded checkpoint.
    pub fn rewind_session(
        &self,
        actor_id: &str,
        key: &str,
        fingerprint: &str,
        session_id: &str,
        to_cursor: u64,
        reason: Option<&str>,
    ) -> Result<IdempotentOutcome<SessionRewindOutcome>, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(existing) = lookup_idempotency::<SessionRewindOutcome>(
            &transaction,
            actor_id,
            "session.rewind",
            key,
            fingerprint,
        )? {
            transaction.commit()?;
            return Ok(IdempotentOutcome {
                value: existing,
                created: false,
            });
        }
        let owner: String = transaction
            .query_row(
                "SELECT actor_id FROM rust_sessions WHERE id = ?1",
                [session_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(StoreError::NotFound("session"))?;
        if owner != actor_id {
            return Err(StoreError::ActorMismatch);
        }
        let mut runs_statement = transaction.prepare(
            "SELECT id, status FROM rust_runs WHERE session_id = ?1 AND status != 'rewound' ORDER BY rowid",
        )?;
        let runs = runs_statement
            .query_map([session_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    RunStatus::parse(&row.get::<_, String>(1)?).map_err(|error| {
                        rusqlite::Error::ToSqlConversionFailure(Box::new(error))
                    })?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(runs_statement);
        if runs.iter().any(|(_, status)| !status.is_terminal()) {
            return Err(StoreError::RewindRejected("session_has_live_run"));
        }
        let cursor_i64 = to_cursor.min(i64::MAX as u64) as i64;
        let beyond: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM rust_runs r JOIN rust_events e ON e.run_id = r.id \
             WHERE r.session_id = ?1 AND r.status != 'rewound' AND e.rowid > ?2",
            params![session_id, cursor_i64],
            |row| row.get(0),
        )?;
        if beyond == 0 {
            return Err(StoreError::RewindRejected("nothing_to_rewind"));
        }
        let run_id = format!("run-{}", Uuid::new_v4());
        let now = timestamp();
        // Collect the retained history prefix before superseding the source
        // runs. The workspace checkpoint is the pre-dispatch snapshot of the
        // FIRST mutating call beyond the cursor — restoring it returns the
        // tree to the state the retained prefix ended in, which keeps changes
        // made by calls whose results are retained (their pre-dispatch
        // snapshot would discard them) and still undoes the first discarded
        // mutation when the cursor sits before it.
        let mut copied = Vec::new();
        let mut checkpoint_ref: Option<String> = None;
        let mut checkpoint_refs: Vec<String> = Vec::new();
        let mut discarded_paths: Vec<String> = Vec::new();
        {
            let mut statement = transaction.prepare(
                "SELECT e.rowid, e.envelope_json FROM rust_runs r JOIN rust_events e ON e.run_id = r.id \
                 WHERE r.session_id = ?1 AND r.status != 'rewound' AND e.rowid <= ?2 ORDER BY r.rowid, e.seq",
            )?;
            let rows = statement.query_map(params![session_id, cursor_i64], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (source_rowid, json) = row?;
                let source: EventEnvelope = serde_json::from_str(&json)?;
                if !is_history_event(&source.event) {
                    continue;
                }
                copied.push((source_rowid, source));
            }
        }
        {
            let mut statement = transaction.prepare(
                "SELECT e.envelope_json FROM rust_runs r JOIN rust_events e ON e.run_id = r.id \
                 WHERE r.session_id = ?1 AND r.status != 'rewound' AND e.rowid > ?2 ORDER BY e.rowid",
            )?;
            let rows = statement.query_map(params![session_id, cursor_i64], |row| {
                row.get::<_, String>(0)
            })?;
            for row in rows {
                let source: EventEnvelope = serde_json::from_str(&row?)?;
                // Path arguments of the discarded file tools — the scoped
                // clean a restore runs so unrelated untracked files survive.
                if let CanonicalEvent::ToolRequested(request) = &source.event
                    && matches!(request.name.as_str(), "write_file" | "edit_file")
                    && let Some(path) = request
                        .arguments
                        .get("path")
                        .and_then(serde_json::Value::as_str)
                {
                    discarded_paths.push(path.to_owned());
                }
                if let Some(value) = source.extensions.get("checkpoint_ref")
                    && let Some(value) = value.as_str()
                {
                    if checkpoint_ref.is_none() {
                        checkpoint_ref = Some(value.to_owned());
                    }
                    checkpoint_refs.push(value.to_owned());
                }
            }
            discarded_paths.sort();
            discarded_paths.dedup();
        }
        let rewound_run_ids: Vec<String> = runs.iter().map(|(id, _)| id.clone()).collect();
        for (existing_id, _) in &runs {
            transaction.execute(
                "UPDATE rust_runs SET status = 'rewound', updated_at = ?1 WHERE id = ?2",
                params![now, existing_id],
            )?;
        }
        transaction.execute(
            "INSERT INTO rust_runs(id, session_id, actor_id, status, last_seq, updated_at) VALUES (?1, ?2, ?3, 'running', 0, ?4)",
            params![run_id, session_id, actor_id, now],
        )?;
        transaction.execute(
            "UPDATE rust_sessions SET active_run_id = ?1 WHERE id = ?2",
            params![run_id, session_id],
        )?;

        let mut next_seq = 0_u64;
        let system_actor = || ActorRef {
            id: "cool-core".to_owned(),
            kind: ActorKind::System,
        };
        let mut rewind_events = vec![EventEnvelope {
            event_id: format!("event-{}", Uuid::new_v4()),
            schema_version: V1Version::VALUE,
            session_id: session_id.to_owned(),
            run_id: run_id.clone(),
            item_id: None,
            seq: 0,
            occurred_at: now.clone(),
            actor: system_actor(),
            source: "cool-state-rewind".to_owned(),
            causation_id: None,
            correlation_id: None,
            event: CanonicalEvent::RunStarted(RunStarted {
                model: None,
                mode: Some("rewind".to_owned()),
            }),
            extensions: Default::default(),
        }];
        for (_, source) in &copied {
            rewind_events.push(EventEnvelope {
                event_id: format!("event-{}", Uuid::new_v4()),
                schema_version: V1Version::VALUE,
                session_id: session_id.to_owned(),
                run_id: run_id.clone(),
                item_id: source.item_id.clone(),
                seq: 0,
                occurred_at: source.occurred_at.clone(),
                actor: source.actor.clone(),
                source: "cool-state-rewind".to_owned(),
                causation_id: Some(source.event_id.clone()),
                correlation_id: source.correlation_id.clone(),
                event: source.event.clone(),
                extensions: source.extensions.clone(),
            });
        }
        rewind_events.push(EventEnvelope {
            event_id: format!("event-{}", Uuid::new_v4()),
            schema_version: V1Version::VALUE,
            session_id: session_id.to_owned(),
            run_id: run_id.clone(),
            item_id: None,
            seq: 0,
            occurred_at: timestamp(),
            actor: system_actor(),
            source: "cool-state-rewind".to_owned(),
            causation_id: None,
            correlation_id: None,
            event: CanonicalEvent::RunRewound(cool_protocol::RunRewound {
                cursor: to_cursor,
                reason: reason.map(str::to_owned),
            }),
            extensions: Default::default(),
        });
        rewind_events.push(EventEnvelope {
            event_id: format!("event-{}", Uuid::new_v4()),
            schema_version: V1Version::VALUE,
            session_id: session_id.to_owned(),
            run_id: run_id.clone(),
            item_id: None,
            seq: 0,
            occurred_at: timestamp(),
            actor: system_actor(),
            source: "cool-state-rewind".to_owned(),
            causation_id: None,
            correlation_id: None,
            event: CanonicalEvent::RunCompleted(RunTerminal {
                reason: "rewind".to_owned(),
                error_code: None,
            }),
            extensions: Default::default(),
        });
        // Same `compact_up_to_cursor` remap as the fork path: source-space
        // cursors would cover nothing once the copies get fresh rowids.
        let mut boundary_rowid = 0_i64;
        let mut inserted_rowids: Vec<(i64, i64)> = Vec::new();
        for (index, event) in rewind_events.iter_mut().enumerate() {
            next_seq += 1;
            event.seq = next_seq;
            if index > 0
                && let CanonicalEvent::SessionCompacted(compacted) = &mut event.event
                && let Some(covered) = compacted.compact_up_to_cursor
            {
                let covered = covered.min(i64::MAX as u64) as i64;
                compacted.compact_up_to_cursor = Some(
                    inserted_rowids
                        .iter()
                        .rev()
                        .find(|(source_rowid, _)| *source_rowid <= covered)
                        .map(|(_, new_rowid)| *new_rowid)
                        .unwrap_or(boundary_rowid)
                        .max(0) as u64,
                );
            }
            append_event_tx(&transaction, actor_id, event)?;
            let new_rowid = transaction.last_insert_rowid();
            if index == 0 {
                boundary_rowid = new_rowid;
            } else if index <= copied.len() {
                inserted_rowids.push((copied[index - 1].0, new_rowid));
            }
        }
        let outcome = SessionRewindOutcome {
            run_id: run_id.clone(),
            rewound_run_ids,
            checkpoint_ref,
            discarded_paths,
            checkpoint_refs,
        };
        insert_idempotency(
            &transaction,
            actor_id,
            "session.rewind",
            key,
            fingerprint,
            &outcome,
        )?;
        transaction.commit()?;
        Ok(IdempotentOutcome {
            value: outcome,
            created: true,
        })
    }

    /// Bind an existing actor-owned session to a legacy conversation (fork
    /// flow): inserts only the link row — no session or transcript import.
    pub fn bind_session_to_conversation(
        &self,
        actor_id: &str,
        key: &str,
        fingerprint: &str,
        conversation_id: i64,
        session_id: &str,
    ) -> Result<ConversationLink, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(existing) = lookup_idempotency::<ConversationLink>(
            &transaction,
            actor_id,
            "session.for_conversation",
            key,
            fingerprint,
        )? {
            transaction.commit()?;
            return Ok(existing);
        }
        if let Some((owner, linked)) = transaction
            .query_row(
                "SELECT actor_id, session_id FROM rust_conversation_links WHERE conversation_id = ?1",
                [conversation_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?
        {
            if owner != actor_id {
                return Err(StoreError::ActorMismatch);
            }
            if linked == session_id {
                let link = ConversationLink {
                    conversation_id,
                    session_id: linked,
                    created: false,
                    imported_events: 0,
                    truncated: false,
                };
                insert_idempotency(
                    &transaction,
                    actor_id,
                    "session.for_conversation",
                    key,
                    fingerprint,
                    &link,
                )?;
                transaction.commit()?;
                return Ok(link);
            }
            return Err(StoreError::IdempotencyConflict);
        }
        let (owner, already_linked): (String, Option<i64>) = transaction
            .query_row(
                "SELECT s.actor_id, (SELECT l.conversation_id FROM rust_conversation_links l \
                 WHERE l.session_id = s.id) FROM rust_sessions s WHERE s.id = ?1",
                [session_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or(StoreError::NotFound("session"))?;
        if owner != actor_id {
            return Err(StoreError::ActorMismatch);
        }
        if already_linked.is_some() {
            return Err(StoreError::IdempotencyConflict);
        }
        let link = ConversationLink {
            conversation_id,
            session_id: session_id.to_owned(),
            created: true,
            imported_events: 0,
            truncated: false,
        };
        transaction.execute(
            "INSERT INTO rust_conversation_links(conversation_id, actor_id, session_id, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![conversation_id, actor_id, session_id, timestamp()],
        )?;
        insert_idempotency(
            &transaction,
            actor_id,
            "session.for_conversation",
            key,
            fingerprint,
            &link,
        )?;
        transaction.commit()?;
        Ok(link)
    }

    /// Find or create the durable session bound to a legacy conversation.
    ///
    /// The first call creates the session and an import run that projects the
    /// supplied legacy transcript into the canonical event log (an import is a
    /// one-way projection, not a dual write: no legacy table is touched). The
    /// link row makes later calls return the same session. Actor-scoped: a link
    /// owned by another actor is rejected instead of being reused.
    #[allow(clippy::too_many_arguments)]
    pub fn link_conversation(
        &self,
        actor_id: &str,
        key: &str,
        fingerprint: &str,
        conversation_id: i64,
        title: Option<&str>,
        project_key: Option<&str>,
        history: &[ImportedHistoryEvent],
        truncated: bool,
    ) -> Result<ConversationLink, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(existing) = lookup_idempotency::<ConversationLink>(
            &transaction,
            actor_id,
            "session.for_conversation",
            key,
            fingerprint,
        )? {
            transaction.commit()?;
            return Ok(existing);
        }
        if let Some((owner, session_id)) = transaction
            .query_row(
                "SELECT actor_id, session_id FROM rust_conversation_links WHERE conversation_id = ?1",
                [conversation_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?
        {
            if owner != actor_id {
                return Err(StoreError::ActorMismatch);
            }
            let link = ConversationLink {
                conversation_id,
                session_id,
                created: false,
                imported_events: 0,
                truncated,
            };
            insert_idempotency(
                &transaction,
                actor_id,
                "session.for_conversation",
                key,
                fingerprint,
                &link,
            )?;
            transaction.commit()?;
            return Ok(link);
        }
        let session_id = format!("session-{}", Uuid::new_v4());
        let run_id = format!("run-{}", Uuid::new_v4());
        let now = timestamp();
        transaction.execute(
            "INSERT INTO rust_sessions(id, actor_id, title, project_key, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![session_id, actor_id, title, project_key, now],
        )?;
        transaction.execute(
            "INSERT INTO rust_runs(id, session_id, actor_id, status, last_seq, updated_at) VALUES (?1, ?2, ?3, 'running', 0, ?4)",
            params![run_id, session_id, actor_id, now],
        )?;
        transaction.execute(
            "UPDATE rust_sessions SET active_run_id = ?1 WHERE id = ?2",
            params![run_id, session_id],
        )?;
        let import_actor = ActorRef {
            id: "cool-core".to_owned(),
            kind: ActorKind::System,
        };
        let mut next_seq = 0_u64;
        let mut append = |event: CanonicalEvent, occurred_at: &str| -> Result<(), StoreError> {
            next_seq += 1;
            let envelope = EventEnvelope {
                event_id: format!("event-{}", Uuid::new_v4()),
                schema_version: V1Version::VALUE,
                session_id: session_id.clone(),
                run_id: run_id.clone(),
                item_id: None,
                seq: next_seq,
                occurred_at: occurred_at.to_owned(),
                actor: import_actor.clone(),
                source: "cool-state-import".to_owned(),
                causation_id: None,
                correlation_id: None,
                event,
                extensions: Default::default(),
            };
            append_event_tx(&transaction, actor_id, &envelope)
        };
        append(
            CanonicalEvent::RunStarted(RunStarted {
                model: None,
                mode: Some("import".to_owned()),
            }),
            &now,
        )?;
        for item in history {
            append(item.event.clone(), &item.occurred_at)?;
        }
        append(
            CanonicalEvent::RunCompleted(RunTerminal {
                reason: "import".to_owned(),
                error_code: None,
            }),
            &now,
        )?;
        let link = ConversationLink {
            conversation_id,
            session_id: session_id.clone(),
            created: true,
            imported_events: history.len() as u64,
            truncated,
        };
        transaction.execute(
            "INSERT INTO rust_conversation_links(conversation_id, actor_id, session_id, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![conversation_id, actor_id, session_id, now],
        )?;
        insert_idempotency(
            &transaction,
            actor_id,
            "session.for_conversation",
            key,
            fingerprint,
            &link,
        )?;
        transaction.commit()?;
        Ok(link)
    }

    /// The durable session bound to one legacy conversation, if any.
    ///
    /// Actor-scoped: a link owned by another actor fails closed with
    /// `ActorMismatch` instead of leaking the session id.
    pub fn session_for_conversation(
        &self,
        actor_id: &str,
        conversation_id: i64,
    ) -> Result<Option<String>, StoreError> {
        let connection = self.connection()?;
        let row: Option<(String, String)> = connection
            .query_row(
                "SELECT actor_id, session_id FROM rust_conversation_links WHERE conversation_id = ?1",
                [conversation_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        match row {
            Some((owner, session_id)) if owner == actor_id => Ok(Some(session_id)),
            Some(_) => Err(StoreError::ActorMismatch),
            None => Ok(None),
        }
    }

    /// The legacy conversation a durable session was linked from, if any.
    ///
    /// Actor-scoped: a link owned by another actor fails closed with
    /// `ActorMismatch`.
    pub fn conversation_id_for_session(
        &self,
        actor_id: &str,
        session_id: &str,
    ) -> Result<Option<i64>, StoreError> {
        let connection = self.connection()?;
        let row: Option<(String, i64)> = connection
            .query_row(
                "SELECT actor_id, conversation_id FROM rust_conversation_links WHERE session_id = ?1",
                [session_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        match row {
            Some((owner, conversation_id)) if owner == actor_id => Ok(Some(conversation_id)),
            Some(_) => Err(StoreError::ActorMismatch),
            None => Ok(None),
        }
    }

    /// Newest-first run summaries of one actor-owned session.
    pub fn list_session_runs(
        &self,
        actor_id: &str,
        session_id: &str,
        limit: usize,
    ) -> Result<Vec<SessionRunEntry>, StoreError> {
        let connection = self.connection()?;
        let owner: String = connection
            .query_row(
                "SELECT actor_id FROM rust_sessions WHERE id = ?1",
                [session_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(StoreError::NotFound("session"))?;
        if owner != actor_id {
            return Err(StoreError::ActorMismatch);
        }
        let mut statement = connection.prepare(
            "SELECT id, status, last_seq, finish_reason, updated_at FROM rust_runs \
             WHERE session_id = ?1 ORDER BY rowid DESC LIMIT ?2",
        )?;
        let rows = statement.query_map(params![session_id, limit as i64], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        let mut runs = Vec::new();
        for row in rows {
            let (run_id, status, last_seq, finish_reason, updated_at) = row?;
            runs.push(SessionRunEntry {
                run_id,
                status: RunStatus::parse(&status)?,
                last_seq: last_seq as u64,
                finish_reason,
                updated_at,
            });
        }
        Ok(runs)
    }

    pub fn steer_run(
        &self,
        actor_id: &str,
        key: &str,
        fingerprint: &str,
        run_id: &str,
        content: &str,
        extensions: Extensions,
    ) -> Result<IdempotentOutcome<SteerAcceptedResult>, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(existing) = lookup_idempotency::<SteerAcceptedResult>(
            &transaction,
            actor_id,
            "session.steer",
            key,
            fingerprint,
        )? {
            transaction.commit()?;
            return Ok(IdempotentOutcome {
                value: existing,
                created: false,
            });
        }
        let run = require_run(&transaction, run_id, actor_id)?;
        if run.status.is_terminal() {
            return Err(StoreError::RunNotActive);
        }
        let event = EventEnvelope {
            event_id: format!("event-{}", Uuid::new_v4()),
            schema_version: V1Version::VALUE,
            session_id: run.session_id,
            run_id: run_id.to_owned(),
            item_id: None,
            seq: run.last_seq + 1,
            occurred_at: timestamp(),
            actor: ActorRef {
                id: actor_id.to_owned(),
                kind: ActorKind::LocalUser,
            },
            source: "cool-state-steer".to_owned(),
            causation_id: None,
            correlation_id: None,
            event: CanonicalEvent::ItemCompleted(ItemEvent {
                role: Some("user".to_owned()),
                content: Some(content.to_owned()),
                tool_calls: Vec::new(),
            }),
            extensions,
        };
        append_event_tx(&transaction, actor_id, &event)?;
        let result = SteerAcceptedResult {
            run_id: run_id.to_owned(),
            seq: event.seq,
        };
        insert_idempotency(
            &transaction,
            actor_id,
            "session.steer",
            key,
            fingerprint,
            &result,
        )?;
        transaction.commit()?;
        Ok(IdempotentOutcome {
            value: result,
            created: true,
        })
    }

    pub fn start_run(
        &self,
        actor_id: &str,
        key: &str,
        fingerprint: &str,
        session_id: &str,
    ) -> Result<IdempotentOutcome<String>, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(existing) = lookup_idempotency::<String>(
            &transaction,
            actor_id,
            "session.prompt",
            key,
            fingerprint,
        )? {
            require_run(&transaction, &existing, actor_id)?;
            transaction.commit()?;
            return Ok(IdempotentOutcome {
                value: existing,
                created: false,
            });
        }
        let (owner, active): (String, Option<String>) = transaction
            .query_row(
                "SELECT actor_id, active_run_id FROM rust_sessions WHERE id = ?1",
                [session_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or(StoreError::NotFound("session"))?;
        if owner != actor_id {
            return Err(StoreError::ActorMismatch);
        }
        if let Some(active_run_id) = active {
            let status = run_status(&transaction, &active_run_id)?;
            if !status.is_terminal() {
                return Err(StoreError::InvalidTransition {
                    from: status,
                    to: RunStatus::Running,
                });
            }
        }
        let run_id = format!("run-{}", Uuid::new_v4());
        transaction.execute(
            "INSERT INTO rust_runs(id, session_id, actor_id, status, last_seq, updated_at) VALUES (?1, ?2, ?3, 'running', 0, ?4)",
            params![run_id, session_id, actor_id, timestamp()],
        )?;
        transaction.execute(
            "UPDATE rust_sessions SET active_run_id = ?1 WHERE id = ?2",
            params![run_id, session_id],
        )?;
        insert_idempotency(
            &transaction,
            actor_id,
            "session.prompt",
            key,
            fingerprint,
            &run_id,
        )?;
        transaction.commit()?;
        Ok(IdempotentOutcome {
            value: run_id,
            created: true,
        })
    }

    /// Create a standalone canonical run for out-of-band lifecycle events
    /// (subagent delegation) that does not become the session's active run and
    /// is not gated by the prompt-run active-run guard.
    ///
    /// The caller owns the run and must append a terminal event to close it;
    /// unlike [`Self::start_run`] this records no idempotency entry because the
    /// auxiliary run is addressed by its own generated id.
    pub fn start_auxiliary_run(
        &self,
        actor_id: &str,
        session_id: &str,
    ) -> Result<String, StoreError> {
        let connection = self.connection()?;
        let owner: String = connection
            .query_row(
                "SELECT actor_id FROM rust_sessions WHERE id = ?1",
                [session_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(StoreError::NotFound("session"))?;
        if owner != actor_id {
            return Err(StoreError::ActorMismatch);
        }
        let run_id = format!("run-{}", Uuid::new_v4());
        connection.execute(
            "INSERT INTO rust_runs(id, session_id, actor_id, status, last_seq, updated_at) VALUES (?1, ?2, ?3, 'running', 0, ?4)",
            params![run_id, session_id, actor_id, timestamp()],
        )?;
        Ok(run_id)
    }

    pub fn lookup_idempotent<T: DeserializeOwned>(
        &self,
        actor_id: &str,
        scope: &str,
        key: &str,
        fingerprint: &str,
    ) -> Result<Option<T>, StoreError> {
        let connection = self.connection()?;
        lookup_idempotency(&connection, actor_id, scope, key, fingerprint)
    }

    pub fn record_idempotent<T: Serialize>(
        &self,
        actor_id: &str,
        scope: &str,
        key: &str,
        fingerprint: &str,
        result: &T,
    ) -> Result<(), StoreError> {
        let connection = self.connection()?;
        insert_idempotency(&connection, actor_id, scope, key, fingerprint, result)
    }

    pub fn run(&self, run_id: &str, actor_id: &str) -> Result<RunSnapshot, StoreError> {
        let connection = self.connection()?;
        let snapshot = connection
            .query_row(
                "SELECT session_id, actor_id, status, last_seq, checkpoint_json FROM rust_runs WHERE id = ?1",
                [run_id],
                |row| {
                    let status: String = row.get(2)?;
                    let checkpoint: Option<String> = row.get(4)?;
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        status,
                        row.get::<_, i64>(3)?,
                        checkpoint,
                    ))
                },
            )
            .optional()?
            .ok_or(StoreError::NotFound("run"))?;
        if snapshot.1 != actor_id {
            return Err(StoreError::ActorMismatch);
        }
        Ok(RunSnapshot {
            run_id: run_id.to_owned(),
            session_id: snapshot.0,
            actor_id: snapshot.1,
            status: RunStatus::parse(&snapshot.2)?,
            last_seq: snapshot.3 as u64,
            checkpoint: snapshot
                .4
                .map(|value| serde_json::from_str(&value))
                .transpose()?,
        })
    }

    pub fn append_event(
        &self,
        owner_actor_id: &str,
        envelope: &EventEnvelope,
    ) -> Result<(), StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let envelope = masked_envelope(envelope.clone())?;
        append_event_tx(&transaction, owner_actor_id, &envelope)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn append_event_auto(
        &self,
        owner_actor_id: &str,
        mut envelope: EventEnvelope,
    ) -> Result<EventEnvelope, StoreError> {
        envelope = masked_envelope(envelope)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let run = require_run(&transaction, &envelope.run_id, owner_actor_id)?;
        envelope.seq = run.last_seq + 1;
        append_event_tx(&transaction, owner_actor_id, &envelope)?;
        transaction.commit()?;
        Ok(envelope)
    }

    pub fn accept_cancel(
        &self,
        actor_id: &str,
        key: &str,
        fingerprint: &str,
        run_id: &str,
        reason: &str,
        provenance: EventProvenance,
    ) -> Result<CancelAcceptance, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(existing) = lookup_idempotency::<RunCancelledResult>(
            &transaction,
            actor_id,
            "run.cancel",
            key,
            fingerprint,
        )? {
            transaction.commit()?;
            return Ok(CancelAcceptance {
                result: existing,
                created: false,
                events: Vec::new(),
            });
        }
        let run = require_run(&transaction, run_id, actor_id)?;
        if run.status.is_terminal() {
            return Err(StoreError::RunNotActive);
        }
        let result = RunCancelledResult {
            run_id: run_id.to_owned(),
            accepted: true,
        };
        insert_idempotency(
            &transaction,
            actor_id,
            "run.cancel",
            key,
            fingerprint,
            &result,
        )?;
        transaction.execute(
            "INSERT INTO rust_cancel_intents(run_id, actor_id, reason, accepted_at) VALUES (?1, ?2, ?3, ?4)",
            params![run_id, actor_id, reason, timestamp()],
        )?;
        let mut events = append_interrupted_tool_failures(
            &transaction,
            actor_id,
            &run.session_id,
            run_id,
            "run_cancelled",
        )?;
        let current = require_run(&transaction, run_id, actor_id)?;
        let event = EventEnvelope {
            event_id: format!("event-{}", Uuid::new_v4()),
            schema_version: V1Version::VALUE,
            session_id: run.session_id,
            run_id: run_id.to_owned(),
            item_id: None,
            seq: current.last_seq + 1,
            occurred_at: timestamp(),
            actor: provenance.actor,
            source: provenance.source,
            causation_id: None,
            correlation_id: None,
            event: CanonicalEvent::RunCancelled(RunTerminal {
                reason: reason.to_owned(),
                error_code: None,
            }),
            extensions: Default::default(),
        };
        append_event_tx(&transaction, actor_id, &event)?;
        events.push(event);
        transaction.commit()?;
        Ok(CancelAcceptance {
            result,
            created: true,
            events,
        })
    }

    pub fn events(
        &self,
        run_id: &str,
        actor_id: &str,
        after_seq: Option<u64>,
        limit: usize,
    ) -> Result<Vec<EventEnvelope>, StoreError> {
        let connection = self.connection()?;
        require_run(&connection, run_id, actor_id)?;
        if after_seq.is_some_and(|value| value > i64::MAX as u64) {
            return Ok(Vec::new());
        }
        let mut statement = connection.prepare(
            "SELECT envelope_json FROM rust_events WHERE run_id = ?1 AND seq > ?2 ORDER BY seq LIMIT ?3",
        )?;
        let rows = statement.query_map(
            params![run_id, after_seq.unwrap_or(0) as i64, limit as i64],
            |row| row.get::<_, String>(0),
        )?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }

    pub fn all_events(
        &self,
        run_id: &str,
        actor_id: &str,
    ) -> Result<Vec<EventEnvelope>, StoreError> {
        self.events(run_id, actor_id, None, usize::MAX)
    }

    pub fn replay_run(&self, run_id: &str, actor_id: &str) -> Result<RunSnapshot, StoreError> {
        let stored = self.run(run_id, actor_id)?;
        let events = self.all_events(run_id, actor_id)?;
        let mut status = RunStatus::Running;
        for (expected, envelope) in (1_u64..).zip(events.iter()) {
            if envelope.seq != expected {
                return Err(StoreError::Corrupt(format!(
                    "event gap for {run_id}: expected {expected}, got {}",
                    envelope.seq
                )));
            }
            status = event_status(&envelope.event).unwrap_or(status);
        }
        if status != stored.status {
            return Err(StoreError::Corrupt(format!(
                "projection status {status:?} differs from row {:?}",
                stored.status
            )));
        }
        Ok(RunSnapshot {
            status,
            last_seq: events.last().map_or(0, |event| event.seq),
            ..stored
        })
    }

    pub fn recover_incomplete_runs(&self) -> Result<Vec<EventEnvelope>, StoreError> {
        let runs = {
            let connection = self.connection()?;
            let mut statement = connection.prepare(
                "SELECT r.id, r.session_id, r.actor_id, c.reason \
                 FROM rust_runs r LEFT JOIN rust_cancel_intents c ON c.run_id = r.id \
                 WHERE r.status IN ('queued', 'running', 'awaiting_approval') ORDER BY r.id",
            )?;
            let rows = statement.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let mut recovered = Vec::new();
        for (run_id, session_id, actor_id, cancel_reason) in runs {
            let mut connection = self.connection()?;
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            append_interrupted_tool_failures(
                &transaction,
                &actor_id,
                &session_id,
                &run_id,
                "run_interrupted",
            )?;
            let current = require_run(&transaction, &run_id, &actor_id)?;
            let event = if let Some(reason) = cancel_reason {
                CanonicalEvent::RunCancelled(RunTerminal {
                    reason,
                    error_code: None,
                })
            } else {
                CanonicalEvent::RunFailed(RunTerminal {
                    reason: "core_restarted".to_owned(),
                    error_code: Some("run_interrupted".to_owned()),
                })
            };
            let envelope = EventEnvelope {
                event_id: format!("event-{}", Uuid::new_v4()),
                schema_version: V1Version::VALUE,
                session_id,
                run_id,
                item_id: None,
                seq: current.last_seq + 1,
                occurred_at: timestamp(),
                actor: ActorRef {
                    id: "cool-core".to_owned(),
                    kind: ActorKind::System,
                },
                source: "cool-core-recovery".to_owned(),
                causation_id: None,
                correlation_id: None,
                event,
                extensions: Default::default(),
            };
            append_event_tx(&transaction, &actor_id, &envelope)?;
            transaction.commit()?;
            recovered.push(envelope);
        }
        Ok(recovered)
    }

    pub fn create_approval(
        &self,
        actor_id: &str,
        session_id: &str,
        run_id: &str,
        call_id: &str,
        tool_name: &str,
        reason: &str,
    ) -> Result<ApprovalTicket, StoreError> {
        self.create_approval_with_arguments(
            actor_id,
            session_id,
            run_id,
            call_id,
            tool_name,
            &BTreeMap::new(),
            reason,
            None,
            None,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_approval_with_arguments(
        &self,
        actor_id: &str,
        session_id: &str,
        run_id: &str,
        call_id: &str,
        tool_name: &str,
        arguments: &BTreeMap<String, serde_json::Value>,
        reason: &str,
        matched_rule: Option<&str>,
        suggested_rule: Option<&cool_protocol::PolicyRuleRecord>,
        breakpoint_type: Option<&str>,
    ) -> Result<ApprovalTicket, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let run = require_run(&transaction, run_id, actor_id)?;
        if run.session_id != session_id {
            return Err(StoreError::ActorMismatch);
        }
        if let Some((approval_id, revision)) = transaction
            .query_row(
                "SELECT id, revision FROM rust_approvals WHERE run_id = ?1 AND call_id = ?2",
                params![run_id, call_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?
        {
            transaction.commit()?;
            return Ok(ApprovalTicket {
                approval_id,
                revision: revision as u64,
                created: false,
            });
        }
        if !transition_allowed(run.status, RunStatus::AwaitingApproval) {
            return Err(StoreError::InvalidTransition {
                from: run.status,
                to: RunStatus::AwaitingApproval,
            });
        }
        let approval_id = format!("approval-{}", Uuid::new_v4());
        transaction.execute(
            "INSERT INTO rust_approvals(id, session_id, run_id, call_id, tool_name, reason, actor_id, revision, state, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1, 'pending', ?8)",
            params![approval_id, session_id, run_id, call_id, tool_name, reason, actor_id, timestamp()],
        )?;
        let event = EventEnvelope {
            event_id: format!("event-{}", Uuid::new_v4()),
            schema_version: V1Version::VALUE,
            session_id: session_id.to_owned(),
            run_id: run_id.to_owned(),
            item_id: None,
            seq: run.last_seq + 1,
            occurred_at: timestamp(),
            actor: ActorRef {
                id: "cool-core".to_owned(),
                kind: ActorKind::System,
            },
            source: "cool-security".to_owned(),
            causation_id: Some(call_id.to_owned()),
            correlation_id: None,
            event: CanonicalEvent::ToolApprovalRequired(Box::new(ToolApprovalRequired {
                call_id: call_id.to_owned(),
                name: tool_name.to_owned(),
                arguments: arguments.clone(),
                reason: reason.to_owned(),
                approval_id: approval_id.clone(),
                revision: 1,
                breakpoint_type: breakpoint_type.map(str::to_owned),
                result_preview: None,
                current_content: None,
                matched_rule: matched_rule.map(str::to_owned),
                suggested_rule: suggested_rule.cloned(),
            })),
            extensions: Default::default(),
        };
        append_event_tx(&transaction, actor_id, &event)?;
        transaction.commit()?;
        Ok(ApprovalTicket {
            approval_id,
            revision: 1,
            created: true,
        })
    }

    /// The committed outcome plus any question answer payload (`ask_user`).
    /// `None` while the approval is still pending.
    pub fn approval_outcome(
        &self,
        actor_id: &str,
        approval_id: &str,
    ) -> Result<Option<(ApprovalOutcome, Option<serde_json::Value>)>, StoreError> {
        let connection = self.connection()?;
        let (owner, state, answer_json) = connection
            .query_row(
                "SELECT actor_id, state, answer_json FROM rust_approvals WHERE id = ?1",
                [approval_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()?
            .ok_or(StoreError::NotFound("approval"))?;
        if owner != actor_id {
            return Err(StoreError::ActorMismatch);
        }
        let answer = answer_json
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .map_err(|error| StoreError::Corrupt(format!("approval answer: {error}")))?;
        match state.as_str() {
            "pending" => Ok(None),
            "approved" => Ok(Some((ApprovalOutcome::Approved, answer))),
            "denied" => Ok(Some((ApprovalOutcome::Denied, answer))),
            "timed_out" => Ok(Some((ApprovalOutcome::TimedOut, answer))),
            _ => Err(StoreError::Corrupt(format!(
                "unknown approval state {state}"
            ))),
        }
    }

    /// The run/session/call an approval ticket belongs to, plus its current
    /// state — used to persist `remember` rules before the resolve commits.
    pub fn approval_call_context(
        &self,
        actor_id: &str,
        approval_id: &str,
    ) -> Result<ApprovalCallContext, StoreError> {
        let connection = self.connection()?;
        let (owner, session_id, run_id, call_id, state) = connection
            .query_row(
                "SELECT actor_id, session_id, run_id, call_id, state FROM rust_approvals WHERE id = ?1",
                [approval_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                },
            )
            .optional()?
            .ok_or(StoreError::NotFound("approval"))?;
        if owner != actor_id {
            return Err(StoreError::ActorMismatch);
        }
        Ok(ApprovalCallContext {
            session_id,
            run_id,
            call_id,
            state,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn resolve_approval(
        &self,
        actor_id: &str,
        key: &str,
        fingerprint: &str,
        approval_id: &str,
        expected_revision: u64,
        decision: ApprovalDecision,
        answer: Option<&serde_json::Value>,
    ) -> Result<ApprovalResolution, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(existing) = lookup_idempotency::<StoredApprovalResolution>(
            &transaction,
            actor_id,
            "approval.resolve",
            key,
            fingerprint,
        )? {
            transaction.commit()?;
            return Ok(existing.into_public(false));
        }
        let approval = transaction
            .query_row(
                "SELECT session_id, run_id, call_id, actor_id, revision, state FROM rust_approvals WHERE id = ?1",
                [approval_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                },
            )
            .optional()?
            .ok_or(StoreError::NotFound("approval"))?;
        if approval.3 != actor_id {
            return Err(StoreError::ActorMismatch);
        }
        if approval.4 as u64 != expected_revision {
            return Err(StoreError::RevisionConflict);
        }
        if approval.5 != "pending" {
            return Err(StoreError::AlreadyResolved);
        }
        let outcome = match decision {
            ApprovalDecision::Approved => ApprovalOutcome::Approved,
            ApprovalDecision::Denied => ApprovalOutcome::Denied,
        };
        let state = match decision {
            ApprovalDecision::Approved => "approved",
            ApprovalDecision::Denied => "denied",
        };
        // `ask_user` answers can be credentials — the durable copy is masked
        // before it touches `answer_json` or the idempotency payload; only
        // the in-memory resolution carries the raw value to the waiting run.
        let masked_answer = answer.map(|answer| {
            let mut value = answer.clone();
            mask_json(&mut value);
            value
        });
        let answer_json = masked_answer
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        let changed = transaction.execute(
            "UPDATE rust_approvals SET state = ?1, revision = revision + 1, decided_by = ?2, decision_source = 'user', decided_at = ?3, answer_json = ?6 \
             WHERE id = ?4 AND actor_id = ?2 AND revision = ?5 AND state = 'pending'",
            params![
                state,
                actor_id,
                timestamp(),
                approval_id,
                expected_revision as i64,
                answer_json
            ],
        )?;
        if changed != 1 {
            return Err(StoreError::RevisionConflict);
        }
        transaction.execute(
            "INSERT INTO rust_audit(id, actor_id, source, action, subject_id, payload_json, occurred_at) VALUES (?1, ?2, 'user', 'approval.resolve', ?3, ?4, ?5)",
            params![format!("audit-{}", Uuid::new_v4()), actor_id, approval_id, serde_json::to_string(&outcome)?, timestamp()],
        )?;
        let run = require_run(&transaction, &approval.1, actor_id)?;
        let event = EventEnvelope {
            event_id: format!("event-{}", Uuid::new_v4()),
            schema_version: V1Version::VALUE,
            session_id: approval.0.clone(),
            run_id: approval.1.clone(),
            item_id: None,
            seq: run.last_seq + 1,
            occurred_at: timestamp(),
            actor: ActorRef {
                id: actor_id.to_owned(),
                kind: ActorKind::LocalUser,
            },
            source: "cool-security".to_owned(),
            causation_id: Some(approval_id.to_owned()),
            correlation_id: None,
            event: CanonicalEvent::ToolApprovalResolved(ToolApprovalResolved {
                call_id: approval.2.clone(),
                approval_id: approval_id.to_owned(),
                revision: expected_revision + 1,
                decision: outcome.clone(),
            }),
            extensions: Default::default(),
        };
        append_event_tx(&transaction, actor_id, &event)?;
        let stored = StoredApprovalResolution {
            approval_id: approval_id.to_owned(),
            run_id: approval.1,
            session_id: approval.0,
            call_id: approval.2,
            revision: expected_revision + 1,
            outcome,
            answer: answer.cloned(),
            event,
        };
        insert_idempotency(
            &transaction,
            actor_id,
            "approval.resolve",
            key,
            fingerprint,
            &StoredApprovalResolution {
                answer: masked_answer.clone(),
                ..stored.clone()
            },
        )?;
        transaction.commit()?;
        Ok(stored.into_public(true))
    }

    /// System-side expiry of a pending approval (P1.8 `question_timeout`):
    /// marks the ticket `timed_out`, audits it, and appends a
    /// `ToolApprovalResolved` event so the run leaves `awaiting_approval`.
    /// Returns `false` when the ticket was already resolved — a racing user
    /// answer then wins and the timeout is a no-op.
    pub fn expire_approval(&self, actor_id: &str, approval_id: &str) -> Result<bool, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let approval = transaction
            .query_row(
                "SELECT session_id, run_id, call_id, actor_id, revision, state FROM rust_approvals WHERE id = ?1",
                [approval_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                },
            )
            .optional()?
            .ok_or(StoreError::NotFound("approval"))?;
        if approval.3 != actor_id {
            return Err(StoreError::ActorMismatch);
        }
        if approval.5 != "pending" {
            transaction.commit()?;
            return Ok(false);
        }
        let changed = transaction.execute(
            "UPDATE rust_approvals SET state = 'timed_out', revision = revision + 1, decision_source = 'system', decided_at = ?1 \
             WHERE id = ?2 AND actor_id = ?3 AND revision = ?4 AND state = 'pending'",
            params![timestamp(), approval_id, actor_id, approval.4],
        )?;
        if changed != 1 {
            transaction.commit()?;
            return Ok(false);
        }
        transaction.execute(
            "INSERT INTO rust_audit(id, actor_id, source, action, subject_id, payload_json, occurred_at) VALUES (?1, ?2, 'system', 'approval.expire', ?3, ?4, ?5)",
            params![
                format!("audit-{}", Uuid::new_v4()),
                actor_id,
                approval_id,
                serde_json::to_string(&ApprovalOutcome::TimedOut)?,
                timestamp()
            ],
        )?;
        let run = require_run(&transaction, &approval.1, actor_id)?;
        let event = EventEnvelope {
            event_id: format!("event-{}", Uuid::new_v4()),
            schema_version: V1Version::VALUE,
            session_id: approval.0.clone(),
            run_id: approval.1.clone(),
            item_id: None,
            seq: run.last_seq + 1,
            occurred_at: timestamp(),
            actor: ActorRef {
                id: actor_id.to_owned(),
                kind: ActorKind::System,
            },
            source: "cool-security".to_owned(),
            causation_id: Some(approval_id.to_owned()),
            correlation_id: None,
            event: CanonicalEvent::ToolApprovalResolved(ToolApprovalResolved {
                call_id: approval.2.clone(),
                approval_id: approval_id.to_owned(),
                revision: approval.4 as u64 + 1,
                decision: ApprovalOutcome::TimedOut,
            }),
            extensions: Default::default(),
        };
        append_event_tx(&transaction, actor_id, &event)?;
        transaction.commit()?;
        Ok(true)
    }

    pub fn set_budget_limits(
        &self,
        actor_id: &str,
        window_key: &str,
        limits: BudgetLimits,
    ) -> Result<(), StoreError> {
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO rust_budgets(actor_id, window_key, limits_json) VALUES (?1, ?2, ?3) \
             ON CONFLICT(actor_id, window_key) DO UPDATE SET limits_json = excluded.limits_json",
            params![actor_id, window_key, serde_json::to_string(&limits)?],
        )?;
        Ok(())
    }

    pub fn reserve_budget(
        &self,
        actor_id: &str,
        window_key: &str,
        delta: BudgetDelta,
    ) -> Result<BudgetSnapshot, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let limits = transaction
            .query_row(
                "SELECT limits_json FROM rust_budgets WHERE actor_id = ?1 AND window_key = ?2",
                params![actor_id, window_key],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|json| serde_json::from_str::<BudgetLimits>(&json))
            .transpose()?
            .unwrap_or_default();
        let current = transaction
            .query_row(
                "SELECT tokens, cost_microusd, iterations, proactive_actions, revision FROM rust_budget_counters WHERE actor_id = ?1 AND window_key = ?2",
                params![actor_id, window_key],
                |row| {
                    Ok(BudgetSnapshot {
                        tokens: row.get::<_, i64>(0)? as u64,
                        cost_microusd: row.get::<_, i64>(1)? as u64,
                        iterations: row.get::<_, i64>(2)? as u64,
                        proactive_actions: row.get::<_, i64>(3)? as u64,
                        revision: row.get::<_, i64>(4)? as u64,
                    })
                },
            )
            .optional()?
            .unwrap_or_default();
        let next = BudgetSnapshot {
            tokens: current
                .tokens
                .checked_add(delta.tokens)
                .ok_or_else(|| StoreError::Corrupt("token counter overflow".to_owned()))?,
            cost_microusd: current
                .cost_microusd
                .checked_add(delta.cost_microusd.unwrap_or(0))
                .ok_or_else(|| StoreError::Corrupt("cost counter overflow".to_owned()))?,
            iterations: current
                .iterations
                .checked_add(delta.iterations)
                .ok_or_else(|| StoreError::Corrupt("iteration counter overflow".to_owned()))?,
            proactive_actions: current
                .proactive_actions
                .checked_add(delta.proactive_actions)
                .ok_or_else(|| StoreError::Corrupt("proactive counter overflow".to_owned()))?,
            revision: current.revision + 1,
        };
        if (limits.cost_microusd.is_some() && delta.cost_microusd.is_none())
            || exceeds(next.tokens, limits.tokens)
            || exceeds(next.cost_microusd, limits.cost_microusd)
            || exceeds(next.iterations, limits.iterations)
            || exceeds(next.proactive_actions, limits.proactive_actions)
        {
            return Err(StoreError::BudgetExceeded(current));
        }
        for (name, value) in [
            ("tokens", next.tokens),
            ("cost", next.cost_microusd),
            ("iterations", next.iterations),
            ("proactive actions", next.proactive_actions),
            ("revision", next.revision),
        ] {
            if value > i64::MAX as u64 {
                return Err(StoreError::Corrupt(format!(
                    "{name} counter exceeds SQLite integer range"
                )));
            }
        }
        transaction.execute(
            "INSERT INTO rust_budget_counters(actor_id, window_key, tokens, cost_microusd, iterations, proactive_actions, revision) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) ON CONFLICT(actor_id, window_key) DO UPDATE SET \
             tokens = excluded.tokens, cost_microusd = excluded.cost_microusd, iterations = excluded.iterations, \
             proactive_actions = excluded.proactive_actions, revision = excluded.revision",
            params![actor_id, window_key, next.tokens as i64, next.cost_microusd as i64, next.iterations as i64, next.proactive_actions as i64, next.revision as i64],
        )?;
        transaction.commit()?;
        Ok(next)
    }

    pub fn add_artifact_reference(&self, artifact: &ArtifactReference) -> Result<(), StoreError> {
        let path = Path::new(&artifact.storage_path);
        let mut components = path.components();
        let first_component = components.next();
        if artifact.sha256.len() != 64
            || !artifact.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
            || artifact.size_bytes > i64::MAX as u64
            || !matches!(first_component, Some(std::path::Component::Normal(_)))
            || !components.all(|component| matches!(component, std::path::Component::Normal(_)))
        {
            return Err(StoreError::Corrupt("invalid artifact reference".to_owned()));
        }
        let session = self.load_session(&artifact.session_id, &artifact.actor_id)?;
        if let Some(run_id) = &artifact.run_id {
            let run = self.run(run_id, &artifact.actor_id)?;
            if run.session_id != session.session_id {
                return Err(StoreError::ActorMismatch);
            }
        }
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO rust_artifact_refs(id, session_id, run_id, sha256, size_bytes, storage_path, actor_id, source, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![artifact.artifact_id, artifact.session_id, artifact.run_id, artifact.sha256, artifact.size_bytes as i64, artifact.storage_path, artifact.actor_id, artifact.source, timestamp()],
        )?;
        Ok(())
    }

    pub fn record_worker_state(
        &self,
        worker_id: &str,
        run_id: Option<&str>,
        status: WorkerStatus,
        generation: u64,
        last_error: Option<&str>,
    ) -> Result<(), StoreError> {
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO rust_workers(id, run_id, status, generation, last_error, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
             ON CONFLICT(id) DO UPDATE SET run_id = excluded.run_id, status = excluded.status, generation = excluded.generation, \
             last_error = excluded.last_error, updated_at = excluded.updated_at \
             WHERE rust_workers.generation <= excluded.generation",
            params![worker_id, run_id, status.as_str(), generation as i64, last_error, timestamp()],
        )?;
        Ok(())
    }

    pub fn begin_worker_generation(
        &self,
        worker_id: &str,
        run_id: Option<&str>,
    ) -> Result<u64, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let generation = transaction
            .query_row(
                "SELECT generation FROM rust_workers WHERE id = ?1",
                [worker_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| StoreError::Corrupt("worker generation overflow".to_owned()))?;
        transaction.execute(
            "INSERT INTO rust_workers(id, run_id, status, generation, last_error, updated_at) VALUES (?1, ?2, 'starting', ?3, NULL, ?4) \
             ON CONFLICT(id) DO UPDATE SET run_id = excluded.run_id, status = 'starting', generation = excluded.generation, \
             last_error = NULL, updated_at = excluded.updated_at",
            params![worker_id, run_id, generation, timestamp()],
        )?;
        transaction.commit()?;
        Ok(generation as u64)
    }

    pub fn worker_generation(&self, worker_id: &str) -> Result<Option<u64>, StoreError> {
        let connection = self.connection()?;
        Ok(connection
            .query_row(
                "SELECT generation FROM rust_workers WHERE id = ?1",
                [worker_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .map(|value| value as u64))
    }
}

#[derive(Clone, Deserialize, Serialize)]
struct StoredApprovalResolution {
    approval_id: String,
    run_id: String,
    session_id: String,
    call_id: String,
    revision: u64,
    outcome: ApprovalOutcome,
    #[serde(default)]
    answer: Option<serde_json::Value>,
    event: EventEnvelope,
}

impl StoredApprovalResolution {
    fn into_public(self, created: bool) -> ApprovalResolution {
        ApprovalResolution {
            approval_id: self.approval_id,
            run_id: self.run_id,
            session_id: self.session_id,
            call_id: self.call_id,
            revision: self.revision,
            outcome: self.outcome,
            answer: self.answer,
            created,
            event: self.event,
        }
    }
}

fn migrate(connection: &Connection) -> Result<(), StoreError> {
    connection.execute_batch(
        "BEGIN IMMEDIATE;
         CREATE TABLE IF NOT EXISTS rust_schema_meta(version INTEGER NOT NULL);
         INSERT INTO rust_schema_meta(version) SELECT 0 WHERE NOT EXISTS (SELECT 1 FROM rust_schema_meta);
         COMMIT;",
    )?;
    let version: i64 =
        connection.query_row("SELECT version FROM rust_schema_meta", [], |row| row.get(0))?;
    if version > SCHEMA_VERSION {
        return Err(StoreError::Corrupt(format!(
            "database schema {version} is newer than supported {SCHEMA_VERSION}"
        )));
    }
    connection.execute_batch(
        "BEGIN IMMEDIATE;
         CREATE TABLE IF NOT EXISTS rust_sessions(
           id TEXT PRIMARY KEY, actor_id TEXT NOT NULL, title TEXT, project_key TEXT,
           active_run_id TEXT REFERENCES rust_runs(id), created_at TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS rust_runs(
           id TEXT PRIMARY KEY, session_id TEXT NOT NULL REFERENCES rust_sessions(id), actor_id TEXT NOT NULL,
           status TEXT NOT NULL CHECK(status IN ('queued','running','awaiting_approval','completed','failed','cancelled','rewound')),
           last_seq INTEGER NOT NULL DEFAULT 0, checkpoint_json TEXT, usage_json TEXT,
           iterations INTEGER NOT NULL DEFAULT 0, finish_reason TEXT, updated_at TEXT NOT NULL
         );
         CREATE UNIQUE INDEX IF NOT EXISTS rust_one_active_run ON rust_sessions(id, active_run_id);
         CREATE TABLE IF NOT EXISTS rust_events(
           event_id TEXT PRIMARY KEY, run_id TEXT NOT NULL REFERENCES rust_runs(id), seq INTEGER NOT NULL,
           envelope_json TEXT NOT NULL, UNIQUE(run_id, seq)
         );
         CREATE TABLE IF NOT EXISTS rust_idempotency(
           actor_id TEXT NOT NULL, scope TEXT NOT NULL, key TEXT NOT NULL, fingerprint TEXT NOT NULL,
           result_json TEXT NOT NULL, PRIMARY KEY(actor_id, scope, key)
         );
         CREATE TABLE IF NOT EXISTS rust_cancel_intents(
           run_id TEXT PRIMARY KEY REFERENCES rust_runs(id), actor_id TEXT NOT NULL,
           reason TEXT NOT NULL, accepted_at TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS rust_approvals(
           id TEXT PRIMARY KEY, session_id TEXT NOT NULL, run_id TEXT NOT NULL REFERENCES rust_runs(id),
           call_id TEXT NOT NULL, tool_name TEXT NOT NULL, reason TEXT NOT NULL, actor_id TEXT NOT NULL,
           revision INTEGER NOT NULL, state TEXT NOT NULL CHECK(state IN ('pending','approved','denied','timed_out')),
           decided_by TEXT, decision_source TEXT, created_at TEXT NOT NULL, decided_at TEXT,
           answer_json TEXT,
           UNIQUE(run_id, call_id)
         );
         CREATE TABLE IF NOT EXISTS rust_audit(
           id TEXT PRIMARY KEY, actor_id TEXT NOT NULL, source TEXT NOT NULL, action TEXT NOT NULL,
           subject_id TEXT NOT NULL, payload_json TEXT NOT NULL, occurred_at TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS rust_budgets(
           actor_id TEXT NOT NULL, window_key TEXT NOT NULL, limits_json TEXT NOT NULL,
           PRIMARY KEY(actor_id, window_key)
         );
         CREATE TABLE IF NOT EXISTS rust_budget_counters(
           actor_id TEXT NOT NULL, window_key TEXT NOT NULL, tokens INTEGER NOT NULL,
           cost_microusd INTEGER NOT NULL, iterations INTEGER NOT NULL, proactive_actions INTEGER NOT NULL,
           revision INTEGER NOT NULL, PRIMARY KEY(actor_id, window_key)
         );
         CREATE TABLE IF NOT EXISTS rust_artifact_refs(
           id TEXT PRIMARY KEY, session_id TEXT NOT NULL REFERENCES rust_sessions(id),
           run_id TEXT REFERENCES rust_runs(id), sha256 TEXT NOT NULL,
           size_bytes INTEGER NOT NULL, storage_path TEXT NOT NULL, actor_id TEXT NOT NULL,
           source TEXT NOT NULL, created_at TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS rust_workers(
           id TEXT PRIMARY KEY, run_id TEXT, status TEXT NOT NULL, generation INTEGER NOT NULL,
           last_error TEXT, updated_at TEXT NOT NULL
         );
         -- Legacy conversation ids live in harness.db, so this is a soft
         -- reference maintained by the app-server, not a cross-database FK.
         CREATE TABLE IF NOT EXISTS rust_conversation_links(
           conversation_id INTEGER PRIMARY KEY, actor_id TEXT NOT NULL,
           session_id TEXT NOT NULL REFERENCES rust_sessions(id), created_at TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS rust_conversation_sessions ON rust_conversation_links(session_id);
         UPDATE rust_schema_meta SET version = 2 WHERE version < 2;
         COMMIT;",
    )?;
    // Question-ask answers (P1.8): nullable JSON column appended to
    // databases created before the DDL above carried it.
    if connection
        .prepare("SELECT answer_json FROM rust_approvals LIMIT 0")
        .is_err()
    {
        connection.execute("ALTER TABLE rust_approvals ADD COLUMN answer_json TEXT", [])?;
    }
    // Run status 'rewound' (P2.13): SQLite cannot ALTER a CHECK constraint,
    // so databases created before the DDL above carried it get rust_runs
    // rebuilt in place — contents preserved, references updated by the rename.
    let runs_ddl: String = connection.query_row(
        "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'rust_runs'",
        [],
        |row| row.get(0),
    )?;
    if !runs_ddl.contains("'rewound'") {
        connection.execute_batch("PRAGMA foreign_keys = OFF")?;
        connection.execute_batch(
            "BEGIN IMMEDIATE;
             CREATE TABLE rust_runs_v3(
               id TEXT PRIMARY KEY, session_id TEXT NOT NULL REFERENCES rust_sessions(id), actor_id TEXT NOT NULL,
               status TEXT NOT NULL CHECK(status IN ('queued','running','awaiting_approval','completed','failed','cancelled','rewound')),
               last_seq INTEGER NOT NULL DEFAULT 0, checkpoint_json TEXT, usage_json TEXT,
               iterations INTEGER NOT NULL DEFAULT 0, finish_reason TEXT, updated_at TEXT NOT NULL
             );
             INSERT INTO rust_runs_v3 SELECT * FROM rust_runs;
             DROP TABLE rust_runs;
             ALTER TABLE rust_runs_v3 RENAME TO rust_runs;
             COMMIT;",
        )?;
        connection.execute_batch("PRAGMA foreign_keys = ON")?;
    }
    connection.execute(
        "UPDATE rust_schema_meta SET version = 3 WHERE version < 3",
        [],
    )?;
    Ok(())
}

fn lookup_idempotency<T: DeserializeOwned>(
    connection: &Connection,
    actor_id: &str,
    scope: &str,
    key: &str,
    fingerprint: &str,
) -> Result<Option<T>, StoreError> {
    let found = connection
        .query_row(
            "SELECT fingerprint, result_json FROM rust_idempotency WHERE actor_id = ?1 AND scope = ?2 AND key = ?3",
            params![actor_id, scope, key],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    let Some((stored_fingerprint, result)) = found else {
        return Ok(None);
    };
    if stored_fingerprint != fingerprint {
        return Err(StoreError::IdempotencyConflict);
    }
    Ok(Some(serde_json::from_str(&result)?))
}

fn insert_idempotency<T: Serialize>(
    connection: &Connection,
    actor_id: &str,
    scope: &str,
    key: &str,
    fingerprint: &str,
    result: &T,
) -> Result<(), StoreError> {
    connection.execute(
        "INSERT INTO rust_idempotency(actor_id, scope, key, fingerprint, result_json) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![actor_id, scope, key, fingerprint, serde_json::to_string(result)?],
    )?;
    Ok(())
}

fn require_run(
    connection: &Connection,
    run_id: &str,
    actor_id: &str,
) -> Result<RunSnapshot, StoreError> {
    let found = connection
        .query_row(
            "SELECT session_id, actor_id, status, last_seq, checkpoint_json FROM rust_runs WHERE id = ?1",
            [run_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            },
        )
        .optional()?
        .ok_or(StoreError::NotFound("run"))?;
    if found.1 != actor_id {
        return Err(StoreError::ActorMismatch);
    }
    Ok(RunSnapshot {
        run_id: run_id.to_owned(),
        session_id: found.0,
        actor_id: found.1,
        status: RunStatus::parse(&found.2)?,
        last_seq: found.3 as u64,
        checkpoint: found
            .4
            .map(|value| serde_json::from_str(&value))
            .transpose()?,
    })
}

fn run_status(connection: &Connection, run_id: &str) -> Result<RunStatus, StoreError> {
    let value = connection
        .query_row(
            "SELECT status FROM rust_runs WHERE id = ?1",
            [run_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .ok_or(StoreError::NotFound("run"))?;
    RunStatus::parse(&value)
}

fn is_history_event(event: &CanonicalEvent) -> bool {
    matches!(
        event,
        CanonicalEvent::ItemCompleted(item)
            if matches!(item.role.as_deref(), Some("user" | "assistant"))
    ) || matches!(
        event,
        CanonicalEvent::ToolCompleted(_) | CanonicalEvent::ToolFailed(_)
    ) || matches!(event, CanonicalEvent::SessionCompacted(_))
}

fn event_status(event: &CanonicalEvent) -> Option<RunStatus> {
    match event {
        CanonicalEvent::RunStarted(_) => Some(RunStatus::Running),
        CanonicalEvent::ToolApprovalRequired(_) => Some(RunStatus::AwaitingApproval),
        // Resolution unblocks the executor. A denied/timed-out tool is not by
        // itself a terminal run fact; the executor records run.cancelled or a
        // normal continuation separately.
        CanonicalEvent::ToolApprovalResolved(_) => Some(RunStatus::Running),
        CanonicalEvent::RunCompleted(_) => Some(RunStatus::Completed),
        CanonicalEvent::RunFailed(_) => Some(RunStatus::Failed),
        CanonicalEvent::RunCancelled(_) => Some(RunStatus::Cancelled),
        _ => None,
    }
}

fn append_interrupted_tool_failures(
    transaction: &Transaction<'_>,
    owner_actor_id: &str,
    session_id: &str,
    run_id: &str,
    error_code: &str,
) -> Result<Vec<EventEnvelope>, StoreError> {
    let mut statement = transaction
        .prepare("SELECT envelope_json FROM rust_events WHERE run_id = ?1 ORDER BY seq")?;
    let rows = statement.query_map([run_id], |row| row.get::<_, String>(0))?;
    let mut pending = BTreeMap::new();
    for row in rows {
        let envelope: EventEnvelope = serde_json::from_str(&row?)?;
        match envelope.event {
            CanonicalEvent::ItemCompleted(item) => {
                for call in item.tool_calls {
                    pending.insert(call.call_id, call.name);
                }
            }
            CanonicalEvent::ToolRequested(call) => {
                pending.insert(call.call_id, call.name);
            }
            CanonicalEvent::ToolCompleted(result) => {
                pending.remove(&result.call_id);
            }
            CanonicalEvent::ToolFailed(result) => {
                pending.remove(&result.call_id);
            }
            _ => {}
        }
    }
    drop(statement);
    let mut appended = Vec::new();
    for (call_id, name) in pending {
        let run = require_run(transaction, run_id, owner_actor_id)?;
        let envelope = EventEnvelope {
            event_id: format!("event-{}", Uuid::new_v4()),
            schema_version: V1Version::VALUE,
            session_id: session_id.to_owned(),
            run_id: run_id.to_owned(),
            item_id: None,
            seq: run.last_seq + 1,
            occurred_at: timestamp(),
            actor: ActorRef {
                id: "cool-core".to_owned(),
                kind: ActorKind::System,
            },
            source: "cool-core-interruption".to_owned(),
            causation_id: Some(call_id.clone()),
            correlation_id: None,
            event: CanonicalEvent::ToolFailed(ToolFailed {
                call_id,
                name,
                error_code: error_code.to_owned(),
                message: Some("tool execution was interrupted before a durable result".to_owned()),
            }),
            extensions: Default::default(),
        };
        append_event_tx(transaction, owner_actor_id, &envelope)?;
        appended.push(envelope);
    }
    Ok(appended)
}

fn append_event_tx(
    transaction: &Transaction<'_>,
    owner_actor_id: &str,
    envelope: &EventEnvelope,
) -> Result<(), StoreError> {
    if envelope.seq > i64::MAX as u64 {
        return Err(StoreError::Corrupt(
            "event sequence exceeds SQLite integer range".to_owned(),
        ));
    }
    let run = require_run(transaction, &envelope.run_id, owner_actor_id)?;
    if run.session_id != envelope.session_id {
        return Err(StoreError::Corrupt(
            "event session does not match run".to_owned(),
        ));
    }
    if run.status.is_terminal() {
        return Err(StoreError::InvalidTransition {
            from: run.status,
            to: event_status(&envelope.event).unwrap_or(run.status),
        });
    }
    let expected_seq = run.last_seq + 1;
    if envelope.seq != expected_seq {
        return Err(StoreError::Corrupt(format!(
            "expected event seq {expected_seq}, got {}",
            envelope.seq
        )));
    }
    let next_status = event_status(&envelope.event).unwrap_or(run.status);
    if !transition_allowed(run.status, next_status) {
        return Err(StoreError::InvalidTransition {
            from: run.status,
            to: next_status,
        });
    }
    let checkpoint = serde_json::json!({
        "lastEventId": envelope.event_id,
        "lastSeq": envelope.seq,
        "status": next_status,
    });
    let persisted = masked_envelope(envelope.clone())?;
    // Derive the run summary reason from the masked event so a secret in a
    // cancellation/terminal reason can never outlive it in `rust_runs`.
    let finish_reason = match &persisted.event {
        CanonicalEvent::RunCompleted(terminal)
        | CanonicalEvent::RunFailed(terminal)
        | CanonicalEvent::RunCancelled(terminal) => Some(terminal.reason.clone()),
        _ => None,
    };
    transaction.execute(
        "INSERT INTO rust_events(event_id, run_id, seq, envelope_json) VALUES (?1, ?2, ?3, ?4)",
        params![
            envelope.event_id,
            envelope.run_id,
            envelope.seq as i64,
            serde_json::to_string(&persisted)?
        ],
    )?;
    transaction.execute(
        "UPDATE rust_runs SET status = ?1, last_seq = ?2, checkpoint_json = ?3, finish_reason = COALESCE(?4, finish_reason), updated_at = ?5 WHERE id = ?6",
        params![next_status.as_str(), envelope.seq as i64, serde_json::to_string(&checkpoint)?, finish_reason, timestamp(), envelope.run_id],
    )?;
    if next_status.is_terminal() {
        transaction.execute(
            "UPDATE rust_sessions SET active_run_id = NULL WHERE id = ?1 AND active_run_id = ?2",
            params![envelope.session_id, envelope.run_id],
        )?;
    }
    Ok(())
}

fn masked_envelope(envelope: EventEnvelope) -> Result<EventEnvelope, StoreError> {
    let mut value = serde_json::to_value(envelope)?;
    mask_json(&mut value);
    Ok(serde_json::from_value(value)?)
}

fn exceeds(value: u64, limit: Option<u64>) -> bool {
    limit.is_some_and(|limit| value > limit)
}

fn timestamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let seconds = now.as_secs();
    let days = (seconds / 86_400) as i64;
    let seconds_of_day = seconds % 86_400;
    let (year, month, day) = civil_date(days);
    let hour = seconds_of_day / 3_600;
    let minute = (seconds_of_day % 3_600) / 60;
    let second = seconds_of_day % 60;
    format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{:03}Z",
        now.subsec_millis()
    )
}

fn civil_date(days_since_epoch: i64) -> (i64, u32, u32) {
    let shifted = days_since_epoch + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    if month <= 2 {
        year += 1;
    }
    (year, month as u32, day as u32)
}

pub fn worker_event(worker_id: &str, attempt: u32, code: Option<String>) -> WorkerEvent {
    WorkerEvent {
        worker_id: worker_id.to_owned(),
        attempt,
        code,
    }
}
