use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::model::{CommandReceipt, Event, RunRecord, SpikeError, SpikeResult};

const APPLICATION_ID: i64 = 0x434F_4F4C;
const SCHEMA_MARKER: &str = "cool-m0-rust-core-spike-v2";
const PROMPT_METHOD: &str = "session.prompt";
const APPROVAL_METHOD: &str = "approval.resolve";

#[derive(Clone, Debug)]
pub struct Store {
    path: PathBuf,
}

#[derive(Debug)]
pub struct RunCreation {
    pub run_id: String,
    pub created: bool,
    pub receipt: Option<CommandReceipt>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApprovalFailpoint {
    AfterApprovalUpdate,
    AfterResolvedEvent,
    AfterEffect,
    BeforeCommit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PromptFailpoint {
    BeforeApprovalInsert,
    AfterApprovalInsert,
    AfterStatusTransition,
    AfterApprovalEvent,
    BeforeCommit,
}

impl PromptFailpoint {
    pub fn parse(value: &str) -> SpikeResult<Self> {
        match value {
            "before-approval-insert" => Ok(Self::BeforeApprovalInsert),
            "after-approval-insert" => Ok(Self::AfterApprovalInsert),
            "after-status-transition" => Ok(Self::AfterStatusTransition),
            "after-approval-event" => Ok(Self::AfterApprovalEvent),
            "before-commit" => Ok(Self::BeforeCommit),
            _ => Err(SpikeError::Protocol(format!(
                "unknown prompt failpoint: {value}"
            ))),
        }
    }

    fn check(self, active: Option<Self>) -> SpikeResult<()> {
        if active == Some(self) {
            return Err(SpikeError::InjectedFailure(format!("{self:?}")));
        }
        Ok(())
    }
}

impl ApprovalFailpoint {
    pub fn parse(value: &str) -> SpikeResult<Self> {
        match value {
            "after-approval-update" => Ok(Self::AfterApprovalUpdate),
            "after-resolved-event" => Ok(Self::AfterResolvedEvent),
            "after-effect" => Ok(Self::AfterEffect),
            "before-commit" => Ok(Self::BeforeCommit),
            _ => Err(SpikeError::Protocol(format!(
                "unknown approval failpoint: {value}"
            ))),
        }
    }

    fn check(self, active: Option<Self>) -> SpikeResult<()> {
        if active == Some(self) {
            return Err(SpikeError::InjectedFailure(format!("{self:?}")));
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct ApprovalRecord {
    pub id: String,
    pub run_id: String,
    pub revision: i64,
    pub status: String,
    pub call_id: String,
    pub tool_name: String,
    pub arguments: Value,
}

#[derive(Debug)]
pub struct AtomicResolution {
    pub receipt: CommandReceipt,
    pub events: Vec<Event>,
    pub replayed: bool,
}

impl Store {
    pub fn create(path: impl AsRef<Path>) -> SpikeResult<Self> {
        let path = path.as_ref().to_path_buf();
        let connection = Connection::open(&path)?;
        connection.busy_timeout(std::time::Duration::from_secs(2))?;
        connection.execute_batch("PRAGMA foreign_keys=ON;")?;
        initialize_or_validate(&connection)?;
        connection.execute_batch("PRAGMA journal_mode=WAL;")?;
        Ok(Self { path })
    }

    fn open(&self) -> SpikeResult<Connection> {
        let connection = Connection::open(&self.path)?;
        connection.busy_timeout(std::time::Duration::from_secs(2))?;
        connection.execute_batch("PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL;")?;
        validate_marker(&connection)?;
        Ok(connection)
    }

    pub fn create_or_get_run(
        &self,
        session_id: &str,
        actor: &str,
        idempotency_key: &str,
        request_hash: &str,
    ) -> SpikeResult<RunCreation> {
        let mut connection = self.open()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some((stored_hash, run_id, result)) =
            command_row(&transaction, actor, PROMPT_METHOD, idempotency_key)?
        {
            if stored_hash != request_hash {
                return Err(SpikeError::IdempotencyConflict);
            }
            let receipt = result
                .map(|value| serde_json::from_str(&value))
                .transpose()?;
            transaction.commit()?;
            return Ok(RunCreation {
                run_id,
                created: false,
                receipt,
            });
        }

        let run_id = Uuid::new_v4().to_string();
        transaction.execute(
            "INSERT INTO runs (id, session_id, actor, status, created_at)
             VALUES (?1, ?2, ?3, 'running', ?4)",
            params![run_id, session_id, actor, now_millis()],
        )?;
        transaction.execute(
            "INSERT INTO commands
             (actor, method, idempotency_key, request_hash, resource_id)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![actor, PROMPT_METHOD, idempotency_key, request_hash, run_id],
        )?;
        transaction.commit()?;
        Ok(RunCreation {
            run_id,
            created: true,
            receipt: None,
        })
    }

    pub fn save_prompt_receipt(
        &self,
        actor: &str,
        idempotency_key: &str,
        receipt: &CommandReceipt,
    ) -> SpikeResult<()> {
        let changed = self.open()?.execute(
            "UPDATE commands SET result = ?1
             WHERE actor = ?2 AND method = ?3 AND idempotency_key = ?4",
            params![
                serde_json::to_string(receipt)?,
                actor,
                PROMPT_METHOD,
                idempotency_key
            ],
        )?;
        if changed != 1 {
            return Err(SpikeError::Protocol(
                "prompt idempotency record missing".to_owned(),
            ));
        }
        Ok(())
    }

    pub fn reconstruct_prompt_receipt(&self, run_id: &str) -> SpikeResult<CommandReceipt> {
        let run = self.run(run_id)?;
        let approval = self
            .open()?
            .query_row(
                "SELECT id, revision FROM approvals WHERE run_id = ?1 ORDER BY rowid DESC LIMIT 1",
                [run_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?;
        Ok(CommandReceipt {
            run_id: run.id,
            status: run.status,
            approval_id: approval.as_ref().map(|value| value.0.clone()),
            approval_revision: approval.map(|value| value.1),
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn append_event(
        &self,
        run_id: &str,
        kind: &str,
        payload: &Value,
        actor: &str,
        source: &str,
        item_id: Option<&str>,
        causation_id: Option<&str>,
    ) -> SpikeResult<Event> {
        let mut connection = self.open()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let event = append_event_tx(
            &transaction,
            run_id,
            kind,
            payload,
            actor,
            source,
            item_id,
            causation_id,
        )?;
        transaction.commit()?;
        Ok(event)
    }

    pub fn list_events(&self, run_id: &str, after_seq: Option<i64>) -> SpikeResult<Vec<Event>> {
        let connection = self.open()?;
        let mut statement = connection.prepare(
            "SELECT event_id, schema_version, session_id, run_id, item_id, seq, occurred_at,
                    actor, source, causation_id, correlation_id, kind, payload
             FROM events WHERE run_id = ?1 AND seq > ?2 ORDER BY seq LIMIT 256",
        )?;
        let rows = statement.query_map(params![run_id, after_seq.unwrap_or(-1)], event_from_row)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn run(&self, run_id: &str) -> SpikeResult<RunRecord> {
        run_tx(&self.open()?, run_id)
    }

    pub fn transition_run(&self, run_id: &str, allowed_from: &[&str], to: &str) -> SpikeResult<()> {
        let connection = self.open()?;
        transition_run_tx(&connection, run_id, allowed_from, to)
    }

    pub fn increment_worker_attempts(&self, run_id: &str) -> SpikeResult<()> {
        self.open()?.execute(
            "UPDATE runs SET worker_attempts = worker_attempts + 1 WHERE id = ?1",
            [run_id],
        )?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn prepare_approval_atomic(
        &self,
        actor: &str,
        idempotency_key: &str,
        run_id: &str,
        call_id: &str,
        tool_name: &str,
        arguments: &Value,
        failpoint: Option<PromptFailpoint>,
    ) -> SpikeResult<(CommandReceipt, Vec<Event>)> {
        let mut connection = self.open()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        PromptFailpoint::BeforeApprovalInsert.check(failpoint)?;
        let record = ApprovalRecord {
            id: Uuid::new_v4().to_string(),
            run_id: run_id.to_owned(),
            revision: 1,
            status: "pending".to_owned(),
            call_id: call_id.to_owned(),
            tool_name: tool_name.to_owned(),
            arguments: arguments.clone(),
        };
        transaction.execute(
            "INSERT INTO approvals
             (id, run_id, revision, status, call_id, tool_name, arguments)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                record.id,
                record.run_id,
                record.revision,
                record.status,
                record.call_id,
                record.tool_name,
                serde_json::to_string(&record.arguments)?,
            ],
        )?;
        PromptFailpoint::AfterApprovalInsert.check(failpoint)?;
        transition_run_tx(&transaction, run_id, &["running"], "awaiting_approval")?;
        PromptFailpoint::AfterStatusTransition.check(failpoint)?;
        let event = append_event_tx(
            &transaction,
            run_id,
            "tool.approval_required",
            &json!({
                "approvalId": record.id,
                "revision": record.revision,
                "callId": call_id,
                "name": tool_name,
                "arguments": arguments,
                "capability": "write"
            }),
            actor,
            "security",
            None,
            Some(call_id),
        )?;
        PromptFailpoint::AfterApprovalEvent.check(failpoint)?;
        let receipt = CommandReceipt {
            run_id: run_id.to_owned(),
            status: "awaiting_approval".to_owned(),
            approval_id: Some(record.id),
            approval_revision: Some(record.revision),
        };
        set_command_receipt_tx(
            &transaction,
            actor,
            PROMPT_METHOD,
            idempotency_key,
            &receipt,
        )?;
        PromptFailpoint::BeforeCommit.check(failpoint)?;
        transaction.commit()?;
        Ok((receipt, vec![event]))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn fail_prompt_atomic(
        &self,
        actor: &str,
        idempotency_key: &str,
        run_id: &str,
        failure_kind: &str,
        source: &str,
        code: &str,
        call_id: Option<&str>,
    ) -> SpikeResult<(CommandReceipt, Vec<Event>)> {
        let mut connection = self.open()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut events = vec![append_event_tx(
            &transaction,
            run_id,
            failure_kind,
            &json!({"callId": call_id, "code": code}),
            actor,
            source,
            None,
            call_id,
        )?];
        transition_run_tx(&transaction, run_id, &["running"], "failed")?;
        events.push(append_event_tx(
            &transaction,
            run_id,
            "run.failed",
            &json!({"code": code}),
            actor,
            "core",
            None,
            call_id,
        )?);
        let receipt = CommandReceipt {
            run_id: run_id.to_owned(),
            status: "failed".to_owned(),
            approval_id: None,
            approval_revision: None,
        };
        set_command_receipt_tx(
            &transaction,
            actor,
            PROMPT_METHOD,
            idempotency_key,
            &receipt,
        )?;
        transaction.commit()?;
        Ok((receipt, events))
    }

    pub fn complete_prompt_atomic(
        &self,
        actor: &str,
        idempotency_key: &str,
        run_id: &str,
    ) -> SpikeResult<(CommandReceipt, Vec<Event>)> {
        let mut connection = self.open()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transition_run_tx(&transaction, run_id, &["running"], "completed")?;
        let event = append_event_tx(
            &transaction,
            run_id,
            "run.completed",
            &json!({"reason": "worker_finished"}),
            actor,
            "core",
            None,
            None,
        )?;
        let receipt = CommandReceipt {
            run_id: run_id.to_owned(),
            status: "completed".to_owned(),
            approval_id: None,
            approval_revision: None,
        };
        set_command_receipt_tx(
            &transaction,
            actor,
            PROMPT_METHOD,
            idempotency_key,
            &receipt,
        )?;
        transaction.commit()?;
        Ok((receipt, vec![event]))
    }

    pub fn recover_incomplete_prompt(
        &self,
        actor: &str,
        idempotency_key: &str,
        run_id: &str,
    ) -> SpikeResult<(CommandReceipt, Vec<Event>)> {
        let run = self.run(run_id)?;
        if run.status == "running" {
            return self.fail_prompt_atomic(
                actor,
                idempotency_key,
                run_id,
                "worker.failed",
                "supervisor",
                "interrupted_before_receipt",
                None,
            );
        }
        let receipt = self.reconstruct_prompt_receipt(run_id)?;
        self.save_prompt_receipt(actor, idempotency_key, &receipt)?;
        Ok((receipt, Vec::new()))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn resolve_approval_atomic(
        &self,
        actor: &str,
        approval_id: &str,
        expected_revision: i64,
        approved: bool,
        idempotency_key: &str,
        request_hash: &str,
        failpoint: Option<ApprovalFailpoint>,
    ) -> SpikeResult<AtomicResolution> {
        let mut connection = self.open()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some((stored_hash, _, result)) =
            command_row(&transaction, actor, APPROVAL_METHOD, idempotency_key)?
        {
            if stored_hash != request_hash {
                return Err(SpikeError::IdempotencyConflict);
            }
            let receipt: CommandReceipt = serde_json::from_str(
                result
                    .as_deref()
                    .ok_or_else(|| SpikeError::Protocol("approval result missing".to_owned()))?,
            )?;
            transaction.commit()?;
            return Ok(AtomicResolution {
                receipt,
                events: Vec::new(),
                replayed: true,
            });
        }

        let mut approval = transaction.query_row(
            "SELECT id, run_id, revision, status, call_id, tool_name, arguments
             FROM approvals WHERE id = ?1",
            [approval_id],
            approval_from_row,
        )?;
        if approval.status != "pending" || approval.revision != expected_revision {
            return Err(SpikeError::StaleApproval);
        }
        transaction.execute(
            "INSERT INTO commands
             (actor, method, idempotency_key, request_hash, resource_id)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                actor,
                APPROVAL_METHOD,
                idempotency_key,
                request_hash,
                approval.run_id
            ],
        )?;
        approval.revision += 1;
        approval.status = if approved { "approved" } else { "denied" }.to_owned();
        transaction.execute(
            "UPDATE approvals SET revision = ?1, status = ?2, decision = ?3 WHERE id = ?4",
            params![
                approval.revision,
                approval.status,
                i64::from(approved),
                approval.id
            ],
        )?;
        ApprovalFailpoint::AfterApprovalUpdate.check(failpoint)?;

        let mut events = vec![append_event_tx(
            &transaction,
            &approval.run_id,
            "approval.resolved",
            &json!({
                "approvalId": approval.id,
                "revision": approval.revision,
                "decision": if approved { "approved" } else { "denied" }
            }),
            actor,
            "user",
            None,
            Some(&approval.call_id),
        )?];
        ApprovalFailpoint::AfterResolvedEvent.check(failpoint)?;

        let status = if !approved {
            events.push(append_event_tx(
                &transaction,
                &approval.run_id,
                "tool.failed",
                &json!({"callId": approval.call_id, "code": "approval_denied"}),
                actor,
                "security",
                None,
                Some(&approval.call_id),
            )?);
            transition_run_tx(
                &transaction,
                &approval.run_id,
                &["awaiting_approval"],
                "completed",
            )?;
            events.push(append_event_tx(
                &transaction,
                &approval.run_id,
                "run.completed",
                &json!({"reason": "approval_denied"}),
                actor,
                "core",
                None,
                Some(&approval.call_id),
            )?);
            "completed"
        } else {
            events.push(append_event_tx(
                &transaction,
                &approval.run_id,
                "tool.started",
                &json!({"callId": approval.call_id, "name": approval.tool_name}),
                actor,
                "core",
                None,
                Some(&approval.call_id),
            )?);
            let effect_hash = hash_json(&json!({
                "tool": approval.tool_name,
                "arguments": approval.arguments
            }))?;
            let existing_effect = transaction
                .query_row(
                    "SELECT run_id, tool_name, arguments_hash FROM tool_effects WHERE call_id = ?1",
                    [&approval.call_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    },
                )
                .optional()?;
            let mut effect_conflict = false;
            let applied = if let Some((run_id, tool_name, arguments_hash)) = existing_effect {
                if run_id != approval.run_id
                    || tool_name != approval.tool_name
                    || arguments_hash != effect_hash
                {
                    effect_conflict = true;
                }
                false
            } else {
                let value = approval
                    .arguments
                    .get("value")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        SpikeError::Protocol("validated write_marker value missing".to_owned())
                    })?;
                transaction.execute(
                    "INSERT INTO tool_effects
                     (call_id, run_id, tool_name, arguments_hash, value, applied_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        approval.call_id,
                        approval.run_id,
                        approval.tool_name,
                        effect_hash,
                        value,
                        now_millis()
                    ],
                )?;
                true
            };
            ApprovalFailpoint::AfterEffect.check(failpoint)?;

            if effect_conflict {
                events.push(append_event_tx(
                    &transaction,
                    &approval.run_id,
                    "tool.failed",
                    &json!({
                        "callId": approval.call_id,
                        "code": "effect_identity_conflict"
                    }),
                    actor,
                    "security",
                    None,
                    Some(&approval.call_id),
                )?);
                transition_run_tx(
                    &transaction,
                    &approval.run_id,
                    &["awaiting_approval"],
                    "failed",
                )?;
                events.push(append_event_tx(
                    &transaction,
                    &approval.run_id,
                    "run.failed",
                    &json!({"code": "effect_identity_conflict"}),
                    actor,
                    "core",
                    None,
                    Some(&approval.call_id),
                )?);
                "failed"
            } else {
                events.push(append_event_tx(
                    &transaction,
                    &approval.run_id,
                    "tool.completed",
                    &json!({
                        "callId": approval.call_id,
                        "name": approval.tool_name,
                        "applied": applied
                    }),
                    actor,
                    "trusted-tool",
                    None,
                    Some(&approval.call_id),
                )?);
                transition_run_tx(
                    &transaction,
                    &approval.run_id,
                    &["awaiting_approval"],
                    "completed",
                )?;
                events.push(append_event_tx(
                    &transaction,
                    &approval.run_id,
                    "run.completed",
                    &json!({"reason": "tool_completed"}),
                    actor,
                    "core",
                    None,
                    Some(&approval.call_id),
                )?);
                "completed"
            }
        };

        let receipt = CommandReceipt {
            run_id: approval.run_id.clone(),
            status: status.to_owned(),
            approval_id: Some(approval.id),
            approval_revision: Some(approval.revision),
        };
        transaction.execute(
            "UPDATE commands SET result = ?1
             WHERE actor = ?2 AND method = ?3 AND idempotency_key = ?4",
            params![
                serde_json::to_string(&receipt)?,
                actor,
                APPROVAL_METHOD,
                idempotency_key
            ],
        )?;
        ApprovalFailpoint::BeforeCommit.check(failpoint)?;
        transaction.commit()?;
        Ok(AtomicResolution {
            receipt,
            events,
            replayed: false,
        })
    }
}

fn initialize_or_validate(connection: &Connection) -> SpikeResult<()> {
    let application_id: i64 =
        connection.query_row("PRAGMA application_id", [], |row| row.get(0))?;
    let object_count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE name NOT LIKE 'sqlite_%'",
        [],
        |row| row.get(0),
    )?;
    if application_id == 0 && object_count == 0 {
        connection.execute_batch(&format!(
            "
            PRAGMA application_id={APPLICATION_ID};
            CREATE TABLE spike_meta (marker TEXT PRIMARY KEY);
            INSERT INTO spike_meta(marker) VALUES ('{SCHEMA_MARKER}');
            CREATE TABLE runs (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                actor TEXT NOT NULL,
                status TEXT NOT NULL,
                next_seq INTEGER NOT NULL DEFAULT 0,
                worker_attempts INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL
            );
            CREATE TABLE commands (
                actor TEXT NOT NULL,
                method TEXT NOT NULL,
                idempotency_key TEXT NOT NULL,
                request_hash TEXT NOT NULL,
                resource_id TEXT NOT NULL,
                result TEXT,
                PRIMARY KEY(actor, method, idempotency_key)
            );
            CREATE TABLE events (
                event_id TEXT PRIMARY KEY,
                schema_version INTEGER NOT NULL,
                session_id TEXT NOT NULL,
                run_id TEXT NOT NULL REFERENCES runs(id),
                item_id TEXT,
                seq INTEGER NOT NULL,
                occurred_at INTEGER NOT NULL,
                actor TEXT NOT NULL,
                source TEXT NOT NULL,
                causation_id TEXT,
                correlation_id TEXT,
                kind TEXT NOT NULL,
                payload TEXT NOT NULL,
                UNIQUE(run_id, seq)
            );
            CREATE TABLE approvals (
                id TEXT PRIMARY KEY,
                run_id TEXT NOT NULL REFERENCES runs(id),
                revision INTEGER NOT NULL,
                status TEXT NOT NULL,
                call_id TEXT NOT NULL,
                tool_name TEXT NOT NULL,
                arguments TEXT NOT NULL,
                decision INTEGER
            );
            CREATE TABLE tool_effects (
                call_id TEXT PRIMARY KEY,
                run_id TEXT NOT NULL REFERENCES runs(id),
                tool_name TEXT NOT NULL,
                arguments_hash TEXT NOT NULL,
                value TEXT NOT NULL,
                applied_at INTEGER NOT NULL
            );
            "
        ))?;
        return Ok(());
    }
    validate_marker(connection)
}

fn validate_marker(connection: &Connection) -> SpikeResult<()> {
    let application_id: i64 =
        connection.query_row("PRAGMA application_id", [], |row| row.get(0))?;
    if application_id != APPLICATION_ID {
        return Err(SpikeError::ForeignDatabase);
    }
    let marker = connection
        .query_row("SELECT marker FROM spike_meta LIMIT 1", [], |row| {
            row.get::<_, String>(0)
        })
        .optional()
        .map_err(|_| SpikeError::ForeignDatabase)?;
    if marker.as_deref() != Some(SCHEMA_MARKER) {
        return Err(SpikeError::ForeignDatabase);
    }
    Ok(())
}

fn command_row(
    transaction: &Transaction<'_>,
    actor: &str,
    method: &str,
    idempotency_key: &str,
) -> SpikeResult<Option<(String, String, Option<String>)>> {
    transaction
        .query_row(
            "SELECT request_hash, resource_id, result FROM commands
             WHERE actor = ?1 AND method = ?2 AND idempotency_key = ?3",
            params![actor, method, idempotency_key],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(Into::into)
}

fn set_command_receipt_tx(
    transaction: &Transaction<'_>,
    actor: &str,
    method: &str,
    idempotency_key: &str,
    receipt: &CommandReceipt,
) -> SpikeResult<()> {
    let changed = transaction.execute(
        "UPDATE commands SET result = ?1
         WHERE actor = ?2 AND method = ?3 AND idempotency_key = ?4",
        params![
            serde_json::to_string(receipt)?,
            actor,
            method,
            idempotency_key
        ],
    )?;
    if changed != 1 {
        return Err(SpikeError::Protocol(
            "command idempotency record missing".to_owned(),
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn append_event_tx(
    transaction: &Transaction<'_>,
    run_id: &str,
    kind: &str,
    payload: &Value,
    actor: &str,
    source: &str,
    item_id: Option<&str>,
    causation_id: Option<&str>,
) -> SpikeResult<Event> {
    let (session_id, seq) = transaction.query_row(
        "SELECT session_id, next_seq FROM runs WHERE id = ?1",
        [run_id],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
    )?;
    let event = Event {
        event_id: Uuid::new_v4().to_string(),
        schema_version: 1,
        session_id,
        run_id: run_id.to_owned(),
        item_id: item_id.map(str::to_owned),
        seq,
        occurred_at: now_millis(),
        actor: actor.to_owned(),
        source: source.to_owned(),
        causation_id: causation_id.map(str::to_owned),
        correlation_id: Some(run_id.to_owned()),
        kind: kind.to_owned(),
        payload: payload.clone(),
    };
    transaction.execute(
        "INSERT INTO events
         (event_id, schema_version, session_id, run_id, item_id, seq, occurred_at,
          actor, source, causation_id, correlation_id, kind, payload)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            event.event_id,
            event.schema_version,
            event.session_id,
            event.run_id,
            event.item_id,
            event.seq,
            event.occurred_at,
            event.actor,
            event.source,
            event.causation_id,
            event.correlation_id,
            event.kind,
            serde_json::to_string(&event.payload)?,
        ],
    )?;
    transaction.execute(
        "UPDATE runs SET next_seq = next_seq + 1 WHERE id = ?1",
        [run_id],
    )?;
    Ok(event)
}

fn event_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Event> {
    let payload: String = row.get(12)?;
    let payload = serde_json::from_str(&payload).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(12, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok(Event {
        event_id: row.get(0)?,
        schema_version: row.get(1)?,
        session_id: row.get(2)?,
        run_id: row.get(3)?,
        item_id: row.get(4)?,
        seq: row.get(5)?,
        occurred_at: row.get(6)?,
        actor: row.get(7)?,
        source: row.get(8)?,
        causation_id: row.get(9)?,
        correlation_id: row.get(10)?,
        kind: row.get(11)?,
        payload,
    })
}

fn approval_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ApprovalRecord> {
    let arguments: String = row.get(6)?;
    let arguments = serde_json::from_str(&arguments).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(6, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok(ApprovalRecord {
        id: row.get(0)?,
        run_id: row.get(1)?,
        revision: row.get(2)?,
        status: row.get(3)?,
        call_id: row.get(4)?,
        tool_name: row.get(5)?,
        arguments,
    })
}

fn run_tx(connection: &Connection, run_id: &str) -> SpikeResult<RunRecord> {
    connection
        .query_row(
            "SELECT id, session_id, actor, status, next_seq, worker_attempts,
                    (SELECT COUNT(*) FROM tool_effects WHERE run_id = runs.id)
             FROM runs WHERE id = ?1",
            [run_id],
            |row| {
                Ok(RunRecord {
                    id: row.get(0)?,
                    session_id: row.get(1)?,
                    actor: row.get(2)?,
                    status: row.get(3)?,
                    next_seq: row.get(4)?,
                    worker_attempts: row.get(5)?,
                    tool_effect_count: row.get(6)?,
                })
            },
        )
        .map_err(Into::into)
}

fn transition_run_tx(
    connection: &Connection,
    run_id: &str,
    allowed_from: &[&str],
    to: &str,
) -> SpikeResult<()> {
    let current: String =
        connection.query_row("SELECT status FROM runs WHERE id = ?1", [run_id], |row| {
            row.get(0)
        })?;
    if !allowed_from.contains(&current.as_str()) {
        return Err(SpikeError::InvalidTransition {
            from: current,
            to: to.to_owned(),
        });
    }
    connection.execute(
        "UPDATE runs SET status = ?1 WHERE id = ?2",
        params![to, run_id],
    )?;
    Ok(())
}

pub fn hash_json(value: &Value) -> SpikeResult<String> {
    let digest = Sha256::digest(serde_json::to_vec(value)?);
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as i64)
}
