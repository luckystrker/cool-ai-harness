//! M6 durable state owned by the Rust trusted core.
//!
//! The tables are deliberately namespaced with `rust_`.  M6 proves the new
//! state/security semantics without dual-writing or taking ownership of the
//! Python application's existing schema; that cutover belongs to M10.

use std::fmt;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use cool_protocol::{
    ActorKind, ActorRef, ApprovalDecision, ApprovalOutcome, CanonicalEvent, EventEnvelope,
    RunCancelledResult, RunTerminal, ToolApprovalRequired, ToolApprovalResolved, V1Version,
    WorkerEvent,
};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use uuid::Uuid;

const SCHEMA_VERSION: i64 = 1;

#[derive(Debug)]
pub enum StoreError {
    Sqlite(rusqlite::Error),
    Json(serde_json::Error),
    Io(std::io::Error),
    InvalidTransition { from: RunStatus, to: RunStatus },
    IdempotencyConflict,
    NotFound(&'static str),
    ActorMismatch,
    RevisionConflict,
    AlreadyResolved,
    RunNotActive,
    BudgetExceeded(BudgetSnapshot),
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
}

impl RunStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::AwaitingApproval => "awaiting_approval",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
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
    pub event: Option<EventEnvelope>,
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
    pub cost_microusd: u64,
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

#[derive(Clone, Debug, PartialEq)]
pub struct ApprovalResolution {
    pub approval_id: String,
    pub run_id: String,
    pub session_id: String,
    pub call_id: String,
    pub revision: u64,
    pub outcome: ApprovalOutcome,
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
        append_event_tx(&transaction, owner_actor_id, envelope)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn append_event_auto(
        &self,
        owner_actor_id: &str,
        mut envelope: EventEnvelope,
    ) -> Result<EventEnvelope, StoreError> {
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
                event: None,
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
        let event = EventEnvelope {
            event_id: format!("event-{}", Uuid::new_v4()),
            schema_version: V1Version::VALUE,
            session_id: run.session_id,
            run_id: run_id.to_owned(),
            item_id: None,
            seq: run.last_seq + 1,
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
        transaction.commit()?;
        Ok(CancelAcceptance {
            result,
            created: true,
            event: Some(event),
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
                "SELECT r.id, r.session_id, r.actor_id, r.last_seq, c.reason \
                 FROM rust_runs r LEFT JOIN rust_cancel_intents c ON c.run_id = r.id \
                 WHERE r.status IN ('queued', 'running', 'awaiting_approval') ORDER BY r.id",
            )?;
            let rows = statement.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let mut recovered = Vec::new();
        for (run_id, session_id, actor_id, last_seq, cancel_reason) in runs {
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
                seq: last_seq as u64 + 1,
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
            self.append_event(&actor_id, &envelope)?;
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
            event: CanonicalEvent::ToolApprovalRequired(ToolApprovalRequired {
                call_id: call_id.to_owned(),
                name: tool_name.to_owned(),
                arguments: Default::default(),
                reason: reason.to_owned(),
                approval_id: approval_id.clone(),
                revision: 1,
                breakpoint_type: None,
                result_preview: None,
                current_content: None,
            }),
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

    pub fn resolve_approval(
        &self,
        actor_id: &str,
        key: &str,
        fingerprint: &str,
        approval_id: &str,
        expected_revision: u64,
        decision: ApprovalDecision,
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
        let changed = transaction.execute(
            "UPDATE rust_approvals SET state = ?1, revision = revision + 1, decided_by = ?2, decision_source = 'user', decided_at = ?3 \
             WHERE id = ?4 AND actor_id = ?2 AND revision = ?5 AND state = 'pending'",
            params![state, actor_id, timestamp(), approval_id, expected_revision as i64],
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
            event,
        };
        insert_idempotency(
            &transaction,
            actor_id,
            "approval.resolve",
            key,
            fingerprint,
            &stored,
        )?;
        transaction.commit()?;
        Ok(stored.into_public(true))
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
                .checked_add(delta.cost_microusd)
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
        if exceeds(next.tokens, limits.tokens)
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

#[derive(Deserialize, Serialize)]
struct StoredApprovalResolution {
    approval_id: String,
    run_id: String,
    session_id: String,
    call_id: String,
    revision: u64,
    outcome: ApprovalOutcome,
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
           status TEXT NOT NULL CHECK(status IN ('queued','running','awaiting_approval','completed','failed','cancelled')),
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
         UPDATE rust_schema_meta SET version = 1 WHERE version < 1;
         COMMIT;",
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
    transaction.execute(
        "INSERT INTO rust_events(event_id, run_id, seq, envelope_json) VALUES (?1, ?2, ?3, ?4)",
        params![
            envelope.event_id,
            envelope.run_id,
            envelope.seq as i64,
            serde_json::to_string(envelope)?
        ],
    )?;
    transaction.execute(
        "UPDATE rust_runs SET status = ?1, last_seq = ?2, checkpoint_json = ?3, updated_at = ?4 WHERE id = ?5",
        params![next_status.as_str(), envelope.seq as i64, serde_json::to_string(&checkpoint)?, timestamp(), envelope.run_id],
    )?;
    if next_status.is_terminal() {
        transaction.execute(
            "UPDATE rust_sessions SET active_run_id = NULL WHERE id = ?1 AND active_run_id = ?2",
            params![envelope.session_id, envelope.run_id],
        )?;
    }
    Ok(())
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
