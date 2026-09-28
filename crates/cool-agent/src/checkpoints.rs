//! P2.18: filesystem checkpoints before workspace-mutating tool calls.
//!
//! A pre-dispatch snapshot is recorded so a later `session.rewind` with
//! `restore_workspace` can return the working tree to the state a tool call
//! saw. Two backends:
//!
//! - Git workspaces: the whole tree is committed to
//!   `refs/cool/checkpoints/{session_id}/{call_seq}` through plumbing
//!   commands (`add`/`write-tree`/`commit-tree`/`update-ref`) run via the
//!   configured `ProcessLauncher` against a private temporary index under
//!   `.cool/checkpoints/`. HEAD, the user's index and every user ref are
//!   never touched. `.cool` itself and gitignored files are not part of the
//!   snapshot.
//! - Non-git workspaces: `.cool/snapshots/{call_seq}.json` records a
//!   manifest plus content copies of the files the tool is about to touch
//!   (`write_file`/`edit_file` `path` arguments). Snapshots taken before a
//!   `shell`/`git` call record the event only — their mutations are opaque
//!   and cannot be restored.
//!
//! A `Disabled` launcher skips the snapshot silently.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cool_security::{Workspace, sanitize_environment};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::io::AsyncReadExt;

use crate::launcher::{LaunchSpec, LauncherKind, NetAccess, ProcessLauncher, ResourceLimits};
use crate::tools::ToolContext;

/// `ToolStarted.extensions` key carrying the checkpoint reference.
pub const CHECKPOINT_EXTENSION_KEY: &str = "checkpoint_ref";

/// `ToolStarted.extensions` key carrying a snapshot failure — recorded so a
/// failed checkpoint is visible in the durable log without blocking the call.
pub const CHECKPOINT_ERROR_EXTENSION_KEY: &str = "checkpoint_error";

/// Tool names that can mutate the workspace and therefore get a
/// pre-dispatch snapshot. `git` mutations are covered by the git backend;
/// in a non-git workspace they record a non-restorable manifest.
const SNAPSHOT_TOOLS: &[&str] = &["write_file", "edit_file", "shell", "git"];

const GIT_REF_PREFIX: &str = "refs/cool/checkpoints";
const MANIFEST_PREFIX: &str = "manifest:";
const SNAPSHOT_DIR: &str = ".cool/snapshots";
const INDEX_DIR: &str = ".cool/checkpoints";
const PLUMBING_TIMEOUT: Duration = Duration::from_secs(60);

/// cap_std roots are `\\?\` verbatim paths on Windows; git's own path
/// handling (env vars, spawn cwd) rejects that prefix, so plumbing paths
/// are normalized back to the plain drive-letter form.
fn plain_path(path: &std::path::Path) -> String {
    let text = path.to_string_lossy().into_owned();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if rest.starts_with(r"UNC\") => format!(r"\\{}", &rest[4..]),
        Some(rest) => rest.to_owned(),
        None => text,
    }
}

/// Whether `tool_name` mutates the workspace and gets a checkpoint.
pub fn tracks_tool(tool_name: &str) -> bool {
    SNAPSHOT_TOOLS.contains(&tool_name)
}

/// Snapshot the workspace before `tool_name` dispatches. `call_seq` is the
/// sequence number of the call's `ToolRequested` event — deterministic and
/// already durable by the time this runs. Returns `Ok(Some(ref))` for the
/// reference stored on `ToolStarted.extensions.checkpoint_ref`; `Ok(None)`
/// when the tool does not mutate the workspace or the launcher is disabled;
/// `Err` when snapshotting failed — a failure never blocks the tool call,
/// the caller records it on `ToolStarted.extensions.checkpoint_error`.
pub async fn snapshot_before_tool(
    context: &ToolContext,
    tool_name: &str,
    call_id: &str,
    call_seq: u64,
    arguments: &Map<String, Value>,
) -> Result<Option<String>, String> {
    if !tracks_tool(tool_name) || context.launcher.kind() == LauncherKind::Disabled {
        return Ok(None);
    }
    let session = context
        .session_id
        .clone()
        .unwrap_or_else(|| "workspace".to_owned());
    let is_git = context.workspace.dir().metadata(".git").is_ok();
    // `seq` is per run — two runs of the same session produce the same
    // ToolRequested seq — so the ref/path is keyed by seq+call_id to keep
    // refs a second run writes from silently retargeting an older run's
    // recorded checkpoint.
    let safe_call_id: String = call_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect();
    let checkpoint_key = format!("{call_seq}-{safe_call_id}");
    if is_git {
        git_snapshot(context, &session, call_id, &checkpoint_key)
            .await
            .map(Some)
    } else {
        manifest_snapshot(context, tool_name, call_id, &checkpoint_key, arguments).map(Some)
    }
}

/// Restore the workspace to `checkpoint_ref`. Git restores run
/// `read-tree`/`checkout-index` against a private index under
/// `.cool/checkpoints/` — the user's index and refs are never touched, so
/// staged changes survive a rewind; the working tree itself is overwritten
/// by design. `tool_paths` names the files the discarded file tools touched
/// — a scoped `git clean` removes exactly those so a rewind deletes the
/// tool's own debris without touching unrelated untracked files the user
/// created after the checkpoint. Shell/git mutations carry no path args and
/// stay non-restorable as documented. Manifest restores rewrite the
/// recorded files; a non-restorable manifest (pre-`shell`/`git` snapshot)
/// is a hard error.
pub async fn restore_checkpoint(
    workspace: &Workspace,
    launcher: &Arc<dyn ProcessLauncher>,
    environment: &HashMap<String, String>,
    checkpoint_ref: &str,
    tool_paths: &[String],
) -> Result<(), String> {
    if let Some(manifest_path) = checkpoint_ref.strip_prefix(MANIFEST_PREFIX) {
        return restore_manifest(workspace, manifest_path);
    }
    if !checkpoint_ref.starts_with(GIT_REF_PREFIX) {
        return Err(format!(
            "unrecognized checkpoint ref '{checkpoint_ref}' (expected {GIT_REF_PREFIX}/…)"
        ));
    }
    workspace
        .dir()
        .create_dir_all(INDEX_DIR)
        .map_err(|error| format!("checkpoint index dir: {error}"))?;
    let index_path = workspace.root().join(INDEX_DIR).join("restore-index");
    let _ = workspace
        .dir()
        .remove_file(format!("{INDEX_DIR}/restore-index"));
    let index_env = [("GIT_INDEX_FILE".to_owned(), plain_path(&index_path))];
    run_plumbing(
        workspace,
        launcher,
        environment,
        &["read-tree", checkpoint_ref],
        &index_env,
    )
    .await?;
    run_plumbing(
        workspace,
        launcher,
        environment,
        &["checkout-index", "-a", "-f"],
        &index_env,
    )
    .await?;
    // Delete only the paths the discarded file tools named — a blanket
    // `git clean` would also remove unrelated files the user created after
    // the checkpoint.
    if !tool_paths.is_empty() {
        let mut args = vec!["clean", "-fd", "-e", ".cool", "--"];
        args.extend(tool_paths.iter().map(String::as_str));
        run_plumbing(workspace, launcher, environment, &args, &index_env).await?;
    }
    Ok(())
}

/// `git add` → `write-tree` → `commit-tree` → `update-ref`, all on a private
/// index file. Returns the ref written under `refs/cool/checkpoints/`.
async fn git_snapshot(
    context: &ToolContext,
    session_id: &str,
    call_id: &str,
    checkpoint_key: &str,
) -> Result<String, String> {
    let workspace = &context.workspace;
    workspace
        .dir()
        .create_dir_all(INDEX_DIR)
        .map_err(|error| format!("checkpoint index dir: {error}"))?;
    let safe_call_id: String = call_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect();
    let index_path = workspace
        .root()
        .join(INDEX_DIR)
        .join(format!("index-{safe_call_id}"));
    let _ = workspace
        .dir()
        .remove_file(format!("{INDEX_DIR}/index-{safe_call_id}"));
    let index_env = [("GIT_INDEX_FILE".to_owned(), plain_path(&index_path))];
    // Seed the private index from HEAD so already-tracked files stay
    // tracked — including paths a later `.gitignore` entry covers, which
    // `add -A` on an empty index would silently skip. No HEAD (unborn
    // branch) starts the index empty; that failure is ignored.
    let _ = run_plumbing(
        workspace,
        &context.launcher,
        &context.environment,
        &["read-tree", "HEAD"],
        &index_env,
    )
    .await;
    // `:(exclude).cool` keeps the runtime's own spill/checkpoint state out
    // of the snapshot; .gitignore still applies to untracked files.
    run_plumbing(
        workspace,
        &context.launcher,
        &context.environment,
        &["add", "-A", "--", ".", ":(exclude).cool"],
        &index_env,
    )
    .await?;
    let tree = run_plumbing(
        workspace,
        &context.launcher,
        &context.environment,
        &["write-tree"],
        &index_env,
    )
    .await?;
    let head = run_plumbing(
        workspace,
        &context.launcher,
        &context.environment,
        &["rev-parse", "--verify", "HEAD"],
        &[],
    )
    .await
    .ok();
    let reference = format!("{GIT_REF_PREFIX}/{session_id}/{checkpoint_key}");
    let mut commit_args = vec!["commit-tree".to_owned(), tree];
    if let Some(head) = head.as_ref().filter(|head| !head.is_empty()) {
        commit_args.push("-p".to_owned());
        commit_args.push(head.clone());
    }
    commit_args.push("-m".to_owned());
    commit_args.push(format!("checkpoint {reference} (call {call_id})"));
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|stamp| stamp.as_secs())
        .unwrap_or(0);
    let ident_env = [
        ("GIT_AUTHOR_NAME".to_owned(), "cool-checkpoint".to_owned()),
        (
            "GIT_AUTHOR_EMAIL".to_owned(),
            "cool-checkpoint@localhost".to_owned(),
        ),
        ("GIT_AUTHOR_DATE".to_owned(), format!("{now} +0000")),
        (
            "GIT_COMMITTER_NAME".to_owned(),
            "cool-checkpoint".to_owned(),
        ),
        (
            "GIT_COMMITTER_EMAIL".to_owned(),
            "cool-checkpoint@localhost".to_owned(),
        ),
        ("GIT_COMMITTER_DATE".to_owned(), format!("{now} +0000")),
    ];
    let commit = run_plumbing(
        workspace,
        &context.launcher,
        &context.environment,
        commit_args
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .as_slice(),
        &ident_env,
    )
    .await?;
    run_plumbing(
        workspace,
        &context.launcher,
        &context.environment,
        &["update-ref", &reference, &commit],
        &[],
    )
    .await?;
    Ok(reference)
}

#[derive(Serialize, Deserialize)]
struct SnapshotManifest {
    version: u32,
    seq: u64,
    call_id: String,
    tool: String,
    restorable: bool,
    files: Vec<ManifestFile>,
}

#[derive(Serialize, Deserialize)]
struct ManifestFile {
    path: String,
    existed: bool,
    snapshot: Option<String>,
}

/// The non-git fallback: a manifest under `.cool/snapshots/` plus content
/// copies of the files the call is about to touch. `shell`/`git` calls are
/// recorded but marked non-restorable.
fn manifest_snapshot(
    context: &ToolContext,
    tool_name: &str,
    call_id: &str,
    checkpoint_key: &str,
    arguments: &Map<String, Value>,
) -> Result<String, String> {
    let dir = context.workspace.dir();
    let snapshot_dir = format!("{SNAPSHOT_DIR}/{checkpoint_key}");
    dir.create_dir_all(format!("{snapshot_dir}/files"))
        .map_err(|error| format!("snapshot dir: {error}"))?;
    let restorable = !matches!(tool_name, "shell" | "git");
    let mut files = Vec::new();
    if restorable {
        let mut paths = Vec::new();
        if let Some(path) = arguments.get("path").and_then(Value::as_str) {
            paths.push(path.to_owned());
        }
        for path in paths {
            let relative = match context.workspace.confine_relative(&path) {
                Ok(relative) => relative,
                Err(_) => continue,
            };
            let existed = dir
                .metadata(&relative)
                .map(|metadata| metadata.is_file())
                .unwrap_or(false);
            let snapshot = if existed {
                let body = dir
                    .read(&relative)
                    .map_err(|error| format!("snapshot read {path}: {error}"))?;
                let name = format!("files/{}", files.len());
                dir.write(format!("{snapshot_dir}/{name}"), &body)
                    .map_err(|error| format!("snapshot write {path}: {error}"))?;
                Some(name)
            } else {
                None
            };
            files.push(ManifestFile {
                path: relative.to_string_lossy().into_owned(),
                existed,
                snapshot,
            });
        }
    }
    let manifest = SnapshotManifest {
        version: 1,
        seq: checkpoint_key
            .split('-')
            .next()
            .and_then(|seq| seq.parse::<u64>().ok())
            .unwrap_or_default(),
        call_id: call_id.to_owned(),
        tool: tool_name.to_owned(),
        restorable,
        files,
    };
    let body = serde_json::to_vec_pretty(&manifest)
        .map_err(|error| format!("manifest serialize: {error}"))?;
    dir.write(format!("{snapshot_dir}/manifest.json"), &body)
        .map_err(|error| format!("manifest write: {error}"))?;
    Ok(format!("{MANIFEST_PREFIX}{snapshot_dir}/manifest.json"))
}

fn restore_manifest(workspace: &Workspace, manifest_path: &str) -> Result<(), String> {
    let dir = workspace.dir();
    let body = dir
        .read(manifest_path)
        .map_err(|error| format!("manifest read: {error}"))?;
    let manifest: SnapshotManifest =
        serde_json::from_slice(&body).map_err(|error| format!("manifest parse: {error}"))?;
    if !manifest.restorable {
        return Err(format!(
            "checkpoint is not restorable (recorded before a {} call)",
            manifest.tool
        ));
    }
    let snapshot_dir = manifest_path
        .rsplit_once('/')
        .map(|(parent, _)| parent.to_owned())
        .unwrap_or_else(|| SNAPSHOT_DIR.to_owned());
    for file in &manifest.files {
        let relative = workspace
            .confine_relative(&file.path)
            .map_err(|_| format!("manifest path escapes workspace: {}", file.path))?;
        match (file.existed, &file.snapshot) {
            (true, Some(snapshot)) => {
                let body = dir
                    .read(format!("{snapshot_dir}/{snapshot}"))
                    .map_err(|error| format!("snapshot read {}: {error}", file.path))?;
                if let Some(parent) = relative.parent() {
                    dir.create_dir_all(parent)
                        .map_err(|error| format!("restore mkdir {parent:?}: {error}"))?;
                }
                dir.write(&relative, &body)
                    .map_err(|error| format!("restore write {}: {error}", file.path))?;
            }
            (false, _) => {
                // Created by the tool call — remove it if it is there now.
                let _ = dir.remove_file(&relative);
            }
            (true, None) => {}
        }
    }
    Ok(())
}

/// `git <args>` through the process launcher — the same gate every tool
/// exec goes through (`Disabled` already rejected by the caller, `Host`
/// runs on the machine, `Sandboxed` binds the workspace read-write).
/// Returns trimmed stdout on success.
async fn run_plumbing(
    workspace: &Workspace,
    launcher: &Arc<dyn ProcessLauncher>,
    environment: &HashMap<String, String>,
    args: &[&str],
    extra_env: &[(String, String)],
) -> Result<String, String> {
    let mut env = sanitize_environment(
        environment
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str())),
        &BTreeSet::new(),
    );
    for (name, value) in extra_env {
        env.insert(name.clone(), value.clone());
    }
    let spec = LaunchSpec {
        cwd: plain_path(workspace.root()).into(),
        env: env.into_iter().collect(),
        stdin: None,
        net: NetAccess::Full,
        limits: ResourceLimits {
            timeout: PLUMBING_TIMEOUT,
            max_output_bytes: 1_048_576,
        },
    };
    let argv: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
    let mut child = launcher
        .spawn("git", &argv, &spec)
        .map_err(|error| format!("git {}: {error}", args.first().unwrap_or(&"")))?;
    let mut stdout = String::new();
    let mut stderr = String::new();
    let mut pipe = child
        .stdout()
        .take()
        .ok_or_else(|| "git plumbing: missing stdout".to_owned())?;
    let stdout_read = tokio::spawn(async move {
        let mut body = Vec::new();
        pipe.read_to_end(&mut body).await.map(|_| body)
    });
    let mut pipe = child
        .stderr()
        .take()
        .ok_or_else(|| "git plumbing: missing stderr".to_owned())?;
    let stderr_read = tokio::spawn(async move {
        let mut body = Vec::new();
        pipe.read_to_end(&mut body).await.map(|_| body)
    });
    let status = match tokio::time::timeout(PLUMBING_TIMEOUT, child.wait()).await {
        Ok(status) => status.map_err(|error| format!("git wait: {error}"))?,
        Err(_) => {
            let _ = std::pin::Pin::from(child.kill()).await;
            let _ = child.wait().await;
            return Err("git plumbing timed out".to_owned());
        }
    };
    if let Ok(Ok(bytes)) = stdout_read.await {
        stdout = String::from_utf8_lossy(&bytes).into_owned();
    }
    if let Ok(Ok(bytes)) = stderr_read.await {
        stderr = String::from_utf8_lossy(&bytes).into_owned();
    }
    if status.success() {
        Ok(stdout.trim().to_owned())
    } else {
        Err(format!(
            "git {} failed ({}): {}",
            args.first().unwrap_or(&""),
            status.code().unwrap_or(-1),
            stderr.trim()
        ))
    }
}
