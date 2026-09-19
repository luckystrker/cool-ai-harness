//! App Protocol dispatch for the legacy React surface families (M10).
//!
//! Every `sdk` command from `frontend/protocol-tests/inventory.json` is served
//! here by a typed `cool-store` call. Transport code never issues SQL: params
//! and records are bridged through serde between the protocol-owned camelCase
//! types and the store-owned camelCase types, and JSON columns stay opaque.
//!
//! Actor scoping comes from `AuthorizedCommand` via the caller (`local_actor`
//! for the local profile); mutations run through
//! `LegacyStore::run_idempotent`, reads use bounded limits.

mod admin;
mod memory;

/// Outcome of a family dispatcher that does not own every command variant.
pub(crate) enum Unhandled {
    NotHandled(Box<Command>),
    Failed(Box<ProtocolError>),
}

impl From<ProtocolError> for Unhandled {
    fn from(error: ProtocolError) -> Self {
        Self::Failed(Box::new(error))
    }
}

use std::future::Future;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cool_protocol::*;
use cool_security::SecretKeyring;
use cool_store::domains::conversations::{
    ConversationFilter, ConversationPatch, MessagePage, NewConversation,
};
use cool_store::domains::runs::{NewRun, RunFilter};
use cool_store::{LegacyStore, StoreError, observability};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::json;
use tokio::process::Command as ProcessCommand;
use tokio::time::timeout;

const GIT_TIMEOUT: Duration = Duration::from_secs(5);
const KEEP_RECENT_MESSAGES: usize = 10;

/// Dispatch a legacy-surface command against the opened store.
pub(crate) async fn dispatch(
    store: &LegacyStore,
    secrets: Option<&SecretKeyring>,
    workspace_root: &Path,
    actor: &ActorRef,
    command: Command,
) -> Result<ResponsePayload, ProtocolError> {
    match command {
        Command::ConversationsList(params) => {
            let filter = ConversationFilter {
                include_machine_owned: params.include_machine_owned,
                archived: params.archived,
                pinned: params.pinned,
                folder: params.folder.clone(),
                search: params.search.clone(),
                limit: Some(usize::from(params.limit)),
                offset: params.offset as usize,
            };
            let records = store
                .list_conversations(&actor.id, &filter)
                .map_err(store_error)?;
            Ok(ResponsePayload::ConversationsList(convert(records)?))
        }
        Command::ConversationsCreate(params) => {
            let new: NewConversation = bridge(&params)?;
            let created = idempotent(
                store,
                actor,
                "conversations.create",
                &params.idempotency_key,
                &fingerprint(&params),
                || store.create_conversation(&actor.id, &new),
            )?;
            Ok(ResponsePayload::ConversationsCreated(convert(created)?))
        }
        Command::ConversationsGet(params) => {
            let record = store
                .get_conversation(&actor.id, params.id)
                .map_err(store_error)?;
            Ok(ResponsePayload::ConversationsGot(convert(record)?))
        }
        Command::ConversationsUpdate(params) => {
            let patch: cool_store::domains::conversations::ConversationPatch = bridge(&params)?;
            let conversation_id = params.id;
            let updated = idempotent(
                store,
                actor,
                "conversations.update",
                &params.idempotency_key,
                &fingerprint(&params),
                || store.update_conversation(&actor.id, conversation_id, &patch),
            )?;
            Ok(ResponsePayload::ConversationsUpdated(convert(updated)?))
        }
        Command::ConversationsDelete(params) => {
            let conversation_id = params.id;
            let deleted = idempotent(
                store,
                actor,
                "conversations.delete",
                &params.idempotency_key,
                &fingerprint(&params),
                || {
                    store.delete_conversation(&actor.id, conversation_id)?;
                    Ok(DeletedResult {
                        deleted: conversation_id,
                    })
                },
            )?;
            Ok(ResponsePayload::ConversationsDeleted(deleted))
        }
        Command::ConversationsCompact(params) => {
            let conversation_id = params.id;
            let compacted = idempotent(
                store,
                actor,
                "conversations.compact",
                &params.idempotency_key,
                &fingerprint(&params),
                || compact_conversation(store, &actor.id, conversation_id),
            )?;
            Ok(ResponsePayload::ConversationsCompacted(compacted))
        }
        Command::ConversationsApprovals(params) => {
            let records = store
                .list_approval_audits(
                    &actor.id,
                    params.id,
                    params.run_id,
                    Some(usize::from(params.limit)),
                )
                .map_err(store_error)?;
            Ok(ResponsePayload::ConversationsApprovals(convert(records)?))
        }
        Command::ConversationsSearch(params) => {
            let filter = ConversationFilter {
                include_machine_owned: true,
                search: Some(params.query.clone()),
                limit: Some(usize::from(params.limit)),
                ..ConversationFilter::default()
            };
            let records = store
                .list_conversations(&actor.id, &filter)
                .map_err(store_error)?;
            Ok(ResponsePayload::ConversationsSearched(convert(records)?))
        }
        Command::ConversationsBulk(params) => {
            let keys = (
                params.ids.clone(),
                params.action.clone(),
                params.folder.clone(),
            );
            let affected = idempotent(
                store,
                actor,
                "conversations.bulk",
                &params.idempotency_key,
                &fingerprint(&params),
                || {
                    let (ids, action, folder) = &keys;
                    let mut affected = 0_u64;
                    for conversation_id in ids {
                        let patch = match action.as_str() {
                            "delete" => {
                                if store
                                    .delete_conversation(&actor.id, *conversation_id)
                                    .is_ok()
                                {
                                    affected += 1;
                                }
                                continue;
                            }
                            "archive" => ConversationPatch {
                                is_archived: Some(true),
                                ..Default::default()
                            },
                            "unarchive" => ConversationPatch {
                                is_archived: Some(false),
                                ..Default::default()
                            },
                            "pin" => ConversationPatch {
                                is_pinned: Some(true),
                                ..Default::default()
                            },
                            "unpin" => ConversationPatch {
                                is_pinned: Some(false),
                                ..Default::default()
                            },
                            // Python skips `move` without a folder and unknown
                            // actions without counting them.
                            "move" if folder.is_some() => ConversationPatch {
                                folder: folder.clone(),
                                ..Default::default()
                            },
                            _ => continue,
                        };
                        if store
                            .update_conversation(&actor.id, *conversation_id, &patch)
                            .is_ok()
                        {
                            affected += 1;
                        }
                    }
                    Ok(AffectedResult {
                        affected,
                        action: action.clone(),
                    })
                },
            )?;
            Ok(ResponsePayload::ConversationsBulk(affected))
        }
        Command::RunsList(params) => {
            let filter = RunFilter {
                before_id: params.before_id,
                limit: Some(usize::from(params.limit)),
            };
            let records = store
                .list_runs(&actor.id, params.conversation_id, &filter)
                .map_err(store_error)?;
            Ok(ResponsePayload::RunsListed(convert(records)?))
        }
        Command::RunsGet(params) => {
            let record = store.get_run(&actor.id, params.id).map_err(store_error)?;
            Ok(ResponsePayload::RunsGot(convert(record)?))
        }
        Command::RunsEvents(params) => {
            let records = store
                .list_run_events(
                    &actor.id,
                    params.run_id,
                    params.after_seq,
                    Some(usize::from(params.limit)),
                )
                .map_err(store_error)?;
            Ok(ResponsePayload::RunsEvents(convert(records)?))
        }
        Command::RunsCancel(params) => {
            let run_id = params.id;
            let cancelled = idempotent(
                store,
                actor,
                "runs.cancel",
                &params.idempotency_key,
                &fingerprint(&params),
                || {
                    store.finish_run(
                        &actor.id,
                        run_id,
                        "cancelled",
                        None,
                        None,
                        Some("client_cancel"),
                        None,
                    )
                },
            )?;
            Ok(ResponsePayload::RunsCancelled(convert(cancelled)?))
        }
        Command::InspectorTimeline(params) => {
            let timeline =
                observability::run_timeline(store, &actor.id, params.id).map_err(store_error)?;
            Ok(ResponsePayload::InspectorTimeline(convert(timeline)?))
        }
        Command::InspectorCompare(params) => {
            let comparison = observability::compare_runs(
                store,
                &actor.id,
                params.left_run_id,
                params.right_run_id,
            )
            .map_err(store_error)?;
            Ok(ResponsePayload::InspectorCompared(convert(comparison)?))
        }
        Command::InspectorReplay(params) => {
            let replay = idempotent(
                store,
                actor,
                "inspector.replay",
                &params.idempotency_key,
                &fingerprint(&params),
                || replay_run(store, &actor.id, &params),
            )?;
            Ok(ResponsePayload::InspectorReplayed(replay))
        }
        Command::WorkspaceGitInfo(params) => Ok(ResponsePayload::WorkspaceGitInfo(
            git_info(&params.path).await?,
        )),
        Command::WorkspaceDirectories(params) => Ok(ResponsePayload::WorkspaceDirectories(
            list_directories(params.path.as_deref(), workspace_root)?,
        )),
        Command::WorkspaceRecent(_) => {
            let recent = store
                .recent_working_directories(&actor.id, 10)
                .map_err(store_error)?;
            Ok(ResponsePayload::WorkspaceRecent(RecentDirectoriesRecord {
                recent,
                default: workspace_root.to_string_lossy().into_owned(),
            }))
        }
        Command::WorkspaceGitStatus(params) => Ok(ResponsePayload::WorkspaceGitStatus(
            git_status(&params.path).await?,
        )),
        Command::WorkspaceGitLog(params) => Ok(ResponsePayload::WorkspaceGitLog(
            git_log(&params.path, params.limit).await?,
        )),
        Command::WorkspaceGitBranches(params) => Ok(ResponsePayload::WorkspaceGitBranches(
            git_branches(&params.path).await?,
        )),
        Command::WorkspaceGitCheckout(params) => {
            let path = params.path.clone();
            let branch = params.branch.clone();
            let result = idempotent_async(
                store,
                actor,
                "workspace.git_checkout",
                &params.idempotency_key,
                &fingerprint(&params),
                || git_checkout(&path, &branch),
            )
            .await?;
            Ok(ResponsePayload::WorkspaceGitCheckout(result))
        }
        command => match memory::dispatch(store, actor, command).await {
            Ok(payload) => Ok(payload),
            Err(Unhandled::Failed(error)) => Err(*error),
            Err(Unhandled::NotHandled(command)) => {
                match admin::dispatch(store, secrets, workspace_root, actor, *command).await {
                    Ok(payload) => Ok(payload),
                    Err(Unhandled::Failed(error)) => Err(*error),
                    Err(Unhandled::NotHandled(_)) => {
                        Err(internal_error("unhandled legacy command"))
                    }
                }
            }
        },
    }
}

/// Compaction fallback for the Rust core: older messages are condensed into a
/// deterministic extractive summary stored in `working_memory`. The Python
/// facade summarizes with an LLM; M11 replaces this fallback with the provider
/// runtime while the message count/cutoff semantics stay identical.
fn compact_conversation(
    store: &LegacyStore,
    actor_id: &str,
    conversation_id: i64,
) -> Result<CompactResult, StoreError> {
    let page = MessagePage {
        before_id: None,
        after_id: None,
        limit: Some(500),
    };
    let messages = store.list_messages(actor_id, conversation_id, &page)?;
    let message_count = messages.len() as u64;
    if messages.len() <= KEEP_RECENT_MESSAGES {
        return Ok(CompactResult {
            status: "skipped".to_owned(),
            reason: Some(format!(
                "Too few messages ({message_count} < {})",
                KEEP_RECENT_MESSAGES + 1
            )),
            message_count: Some(message_count),
            messages_compacted: None,
            messages_kept: None,
            summary_length: None,
        });
    }
    let existing = store.get_working_memory(actor_id, conversation_id)?;
    let previous_cutoff = existing
        .as_ref()
        .filter(|memory| memory.summary.is_some())
        .and_then(|memory| memory.summary_up_to_message_id);
    let older = &messages[..messages.len() - KEEP_RECENT_MESSAGES];
    let fresh = older
        .iter()
        .filter(|message| previous_cutoff.is_none_or(|cutoff| message.id > cutoff))
        .collect::<Vec<_>>();
    if fresh.is_empty() {
        return Ok(CompactResult {
            status: "skipped".to_owned(),
            reason: Some("No new messages to compact".to_owned()),
            message_count: Some(message_count),
            messages_compacted: None,
            messages_kept: Some(KEEP_RECENT_MESSAGES as u64),
            summary_length: None,
        });
    }
    let mut transcript = String::new();
    if let Some(memory) = &existing
        && let Some(summary) = &memory.summary
    {
        transcript.push_str("[Previous summary of the earlier conversation]\n");
        transcript.push_str(summary);
        transcript.push('\n');
    }
    for message in &fresh {
        let content = message.content.clone().unwrap_or_default();
        let content = if content.chars().count() > 300 {
            format!("{}...", content.chars().take(300).collect::<String>())
        } else {
            content
        };
        transcript.push_str(&format!("{}: {}\n", message.role, content));
    }
    let state = existing
        .as_ref()
        .map(|memory| memory.state.clone())
        .unwrap_or_else(|| json!({}));
    let last_compacted = fresh.last().map(|message| message.id);
    let summary_length = transcript.chars().count() as i64;
    store.upsert_working_memory(
        actor_id,
        conversation_id,
        &state,
        Some(&transcript),
        last_compacted,
        Some(summary_length),
    )?;
    Ok(CompactResult {
        status: "compacted".to_owned(),
        reason: None,
        message_count: Some(message_count),
        messages_compacted: Some(fresh.len() as u64),
        messages_kept: Some(KEEP_RECENT_MESSAGES as u64),
        summary_length: Some(summary_length as u64),
    })
}

fn replay_run(
    store: &LegacyStore,
    actor_id: &str,
    params: &ReplayParams,
) -> Result<ReplayResult, StoreError> {
    let run = store.get_run(actor_id, params.run_id)?;
    let page = MessagePage {
        before_id: None,
        after_id: None,
        limit: Some(500),
    };
    let messages = store.list_messages(actor_id, run.conversation_id, &page)?;
    let user_input = messages
        .iter()
        .filter(|message| message.role == "user" && message.created_at <= run.started_at)
        .max_by(|left, right| left.created_at.cmp(&right.created_at))
        .and_then(|message| message.content.clone());
    if let Some(input) = user_input {
        store.add_message(
            actor_id,
            run.conversation_id,
            &cool_store::domains::conversations::NewMessage {
                role: "user".to_owned(),
                content: Some(format!("[Replay] {input}")),
                ..Default::default()
            },
        )?;
    }
    let config = json!({
        "replay_of": params.run_id,
        "system_prompt_override": params.system_prompt,
        "temperature_override": params.temperature,
    });
    let new_run = store.create_run(
        actor_id,
        run.conversation_id,
        &NewRun {
            model: params.model.clone().or_else(|| run.model.clone()),
            config: Some(config),
        },
    )?;
    Ok(ReplayResult {
        new_run_id: new_run.id,
        original_run_id: params.run_id,
        status: new_run.status,
    })
}

// --- Workspace (filesystem + git capability) --------------------------------

fn list_directories(
    path: Option<&str>,
    workspace_root: &Path,
) -> Result<DirectoryListingRecord, ProtocolError> {
    let target = match path {
        Some(path) if !path.is_empty() => std::path::PathBuf::from(path),
        _ => workspace_root.to_path_buf(),
    };
    if !target.is_dir() {
        return Err(invalid_input("path is not a directory"));
    }
    let mut directories = Vec::new();
    let entries = std::fs::read_dir(&target).map_err(|error| {
        protocol_error(-32603, "workspace_read_failed", error.to_string(), false)
    })?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            directories.push(name);
        }
    }
    directories.sort_by_key(|name| name.to_lowercase());
    let parent = target
        .parent()
        .filter(|parent| *parent != target)
        .map(|parent| parent.to_string_lossy().into_owned());
    Ok(DirectoryListingRecord {
        current: target.to_string_lossy().into_owned(),
        parent,
        directories,
        default: workspace_root.to_string_lossy().into_owned(),
    })
}

async fn git_output(path: &str, args: &[&str]) -> Result<(bool, String, String), ProtocolError> {
    let target = Path::new(path);
    if !target.is_dir() {
        return Err(invalid_input("path is not a directory"));
    }
    let result = timeout(
        GIT_TIMEOUT,
        ProcessCommand::new("git")
            .args(args)
            .current_dir(target)
            .output(),
    )
    .await
    .map_err(|_| protocol_error(-32000, "git_timeout", "git command timed out", true))?
    .map_err(|error| protocol_error(-32000, "git_unavailable", error.to_string(), false))?;
    Ok((
        result.status.success(),
        String::from_utf8_lossy(&result.stdout).into_owned(),
        String::from_utf8_lossy(&result.stderr).into_owned(),
    ))
}

async fn git_info(path: &str) -> Result<GitInfoRecord, ProtocolError> {
    let (ok, stdout, _) = git_output(path, &["rev-parse", "--abbrev-ref", "HEAD"]).await?;
    Ok(GitInfoRecord {
        path: path.to_owned(),
        is_git: ok,
        branch: ok.then(|| stdout.trim().to_owned()),
    })
}

async fn git_status(path: &str) -> Result<GitStatusRecord, ProtocolError> {
    let (ok, stdout, _) = git_output(path, &["status", "--porcelain=v1", "--branch"]).await?;
    if !ok {
        return Ok(GitStatusRecord {
            path: path.to_owned(),
            is_git: false,
            branch: None,
            staged: Vec::new(),
            modified: Vec::new(),
            untracked: Vec::new(),
        });
    }
    let mut staged = Vec::new();
    let mut modified = Vec::new();
    let mut untracked = Vec::new();
    let mut branch = None;
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("## ") {
            branch = Some(rest.split("...").next().unwrap_or(rest).to_owned());
        } else if let Some(file) = line.strip_prefix("?? ") {
            untracked.push(file.to_owned());
        } else if line.len() >= 3 {
            let bytes = line.as_bytes();
            let (x, y) = (bytes[0] as char, bytes[1] as char);
            let file = line[3..].to_owned();
            if matches!(x, 'A' | 'M' | 'R' | 'C') {
                staged.push(file.clone());
            }
            if matches!(y, 'M' | 'D') {
                modified.push(file);
            }
        }
    }
    Ok(GitStatusRecord {
        path: path.to_owned(),
        is_git: branch.is_some(),
        branch,
        staged,
        modified,
        untracked,
    })
}

async fn git_log(path: &str, limit: u16) -> Result<GitLogRecord, ProtocolError> {
    let count = format!("-{}", limit.clamp(1, 100));
    let (ok, stdout, stderr) = git_output(
        path,
        &[
            "log",
            "--pretty=format:%H%x1f%s%x1f%an%x1f%ad",
            "--date=short",
            &count,
        ],
    )
    .await?;
    if !ok {
        return Err(protocol_error(
            -32004,
            "git_not_a_repository",
            stderr,
            false,
        ));
    }
    let commits = stdout
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('\u{1f}');
            Some(GitLogEntryRecord {
                hash: fields.next()?.to_owned(),
                message: fields.next().unwrap_or_default().to_owned(),
                author: fields.next().unwrap_or_default().to_owned(),
                date: fields.next().unwrap_or_default().to_owned(),
            })
        })
        .collect();
    Ok(GitLogRecord {
        path: path.to_owned(),
        commits,
    })
}

async fn git_branches(path: &str) -> Result<GitBranchesRecord, ProtocolError> {
    let (ok, stdout, stderr) = git_output(path, &["branch", "--format=%(refname:short)"]).await?;
    if !ok {
        return Err(protocol_error(
            -32004,
            "git_not_a_repository",
            stderr,
            false,
        ));
    }
    let branches = stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let (head_ok, head, _) = git_output(path, &["rev-parse", "--abbrev-ref", "HEAD"]).await?;
    Ok(GitBranchesRecord {
        path: path.to_owned(),
        branches,
        current: head_ok.then(|| head.trim().to_owned()),
    })
}

async fn git_checkout(path: &str, branch: &str) -> Result<GitCheckoutResult, StoreError> {
    if branch.trim().is_empty() {
        return Err(StoreError::InvalidInput(
            "branch must not be empty".to_owned(),
        ));
    }
    let target = Path::new(path);
    if !target.is_dir() {
        return Err(StoreError::InvalidInput(
            "path is not a directory".to_owned(),
        ));
    }
    let result = timeout(
        GIT_TIMEOUT,
        ProcessCommand::new("git")
            .args(["checkout", branch])
            .current_dir(target)
            .output(),
    )
    .await
    .map_err(|_| StoreError::InvalidInput("git command timed out".to_owned()))?
    .map_err(|error| StoreError::InvalidInput(format!("git unavailable: {error}")))?;
    if !result.status.success() {
        return Err(StoreError::InvalidInput(
            String::from_utf8_lossy(&result.stderr).into_owned(),
        ));
    }
    Ok(GitCheckoutResult {
        path: path.to_owned(),
        branch: branch.to_owned(),
        status: "checked_out".to_owned(),
    })
}

// --- Shared helpers ---------------------------------------------------------

pub(crate) fn protocol_error(
    rpc_code: i32,
    cool_code: &str,
    message: impl Into<String>,
    retryable: bool,
) -> ProtocolError {
    let mut error = ProtocolError {
        rpc_code,
        cool_code: cool_code.to_owned(),
        message: message.into(),
        retryable,
        safe_details: Default::default(),
    };
    if error.message.is_empty() {
        error.message = cool_code.replace('_', " ");
    }
    error
}

pub(crate) fn invalid_input(message: impl Into<String>) -> ProtocolError {
    protocol_error(-32602, "invalid_input", message, false)
}

pub(crate) fn internal_error(message: impl Into<String>) -> ProtocolError {
    protocol_error(-32603, "internal_error", message, false)
}

/// Bridge a protocol params struct into the matching store struct. Store
/// structs ignore the extra `idempotencyKey`/`id` fields; both sides use
/// camelCase field names, and JSON columns pass through untouched.
pub(crate) fn bridge<P: Serialize, S: DeserializeOwned>(params: &P) -> Result<S, ProtocolError> {
    let value = serde_json::to_value(params).map_err(|error| internal_error(error.to_string()))?;
    serde_json::from_value(value).map_err(|error| invalid_input(error.to_string()))
}

/// Convert a store record into its protocol mirror.
pub(crate) fn convert<S: Serialize, P: DeserializeOwned>(value: S) -> Result<P, ProtocolError> {
    let value = serde_json::to_value(value).map_err(|error| internal_error(error.to_string()))?;
    serde_json::from_value(value).map_err(|error| internal_error(error.to_string()))
}

pub(crate) fn fingerprint<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| String::new())
}

pub(crate) fn idempotent<T, F>(
    store: &LegacyStore,
    actor: &ActorRef,
    method: &str,
    key: &IdempotencyKey,
    fingerprint: &str,
    action: F,
) -> Result<T, ProtocolError>
where
    T: Serialize + DeserializeOwned,
    F: FnOnce() -> Result<T, StoreError>,
{
    store
        .run_idempotent(&actor.id, method, key.as_str(), fingerprint, action)
        .map(|outcome| outcome.value)
        .map_err(store_error)
}

pub(crate) async fn idempotent_async<T, F, Fut>(
    store: &LegacyStore,
    actor: &ActorRef,
    method: &str,
    key: &IdempotencyKey,
    fingerprint: &str,
    action: F,
) -> Result<T, ProtocolError>
where
    T: Serialize + DeserializeOwned,
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<T, StoreError>>,
{
    store
        .run_idempotent_async(&actor.id, method, key.as_str(), fingerprint, action)
        .await
        .map(|outcome| outcome.value)
        .map_err(store_error)
}

pub(crate) fn store_error(error: StoreError) -> ProtocolError {
    match error {
        StoreError::NotFound(kind) => protocol_error(
            -32004,
            &format!("{}_not_found", kind.replace(' ', "_")),
            format!("{kind} not found"),
            false,
        ),
        StoreError::InvalidInput(message) => invalid_input(message),
        StoreError::Conflict(message) => protocol_error(-32006, "conflict", message, false),
        StoreError::ActorUnknown(actor) => protocol_error(
            -32005,
            "actor_unknown",
            format!("unknown actor {actor}"),
            false,
        ),
        StoreError::Corruption(message) => {
            protocol_error(-32603, "legacy_store_corrupt", message, true)
        }
        StoreError::NotALegacyStore
        | StoreError::UnsupportedLegacyRevision { .. }
        | StoreError::MigrationOwnershipConflict(_) => protocol_error(
            -32010,
            "legacy_store_unavailable",
            "legacy store is not available to the Rust runtime",
            false,
        ),
        other => protocol_error(-32603, "legacy_store_error", other.to_string(), true),
    }
}

pub(crate) fn encrypt_secret(
    secrets: Option<&SecretKeyring>,
    plaintext: &str,
) -> Result<String, ProtocolError> {
    let keyring = secrets.ok_or_else(|| {
        protocol_error(
            -32010,
            "secret_key_unavailable",
            "a secret keyring is required to store provider credentials",
            false,
        )
    })?;
    keyring.encrypt(plaintext).map_err(|error| {
        protocol_error(-32603, "secret_encryption_failed", error.to_string(), false)
    })
}

pub(crate) fn now_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
}
