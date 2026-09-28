//! P2.18: pre-dispatch filesystem checkpoints — git-plumbing backend and the
//! `.cool/snapshots` manifest fallback, plus the `restore_checkpoint` paths
//! `session.rewind restore_workspace` uses.

use std::collections::HashMap;
use std::process::Command;
use std::sync::Arc;

use cool_agent::{
    DisabledLauncher, HostLauncher, ToolContext, restore_checkpoint, snapshot_before_tool,
    tracks_tool,
};
use cool_security::{CapabilityPolicy, Decision, Workspace};
use serde_json::{Map, Value, json};
use tempfile::tempdir;

fn git(dir: &std::path::Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn git_repo() -> tempfile::TempDir {
    let dir = tempdir().unwrap();
    git(dir.path(), &["init", "-b", "main"]);
    git(
        dir.path(),
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "--allow-empty",
            "-m",
            "init",
        ],
    );
    dir
}

fn context(dir: &std::path::Path) -> ToolContext {
    ToolContext::new(
        Workspace::new(dir).unwrap(),
        CapabilityPolicy::new(Some(Decision::Allow)),
    )
    .with_session_id("session-1")
    .with_launcher(Arc::new(HostLauncher))
    .with_environment(std::env::vars().collect::<HashMap<_, _>>())
}

fn args(map: Map<String, Value>) -> Map<String, Value> {
    map
}

#[tokio::test]
async fn git_snapshot_uses_private_index_and_never_touches_head_or_index() {
    let dir = git_repo();
    std::fs::write(dir.path().join("a.txt"), "v1").unwrap();
    // A staged change in the user's index must survive the snapshot untouched.
    git(dir.path(), &["add", "a.txt"]);
    let head_before = git(dir.path(), &["rev-parse", "HEAD"]);
    let index_before = git(dir.path(), &["ls-files", "--stage"]);

    let context = context(dir.path());
    let reference = snapshot_before_tool(
        &context,
        "write_file",
        "call-1",
        9,
        &args(serde_json::Map::from_iter([(
            "path".to_owned(),
            json!("a.txt"),
        )])),
    )
    .await
    .unwrap()
    .expect("checkpoint ref");
    // `seq` is per run — the call id disambiguates refs across runs.
    assert_eq!(reference, "refs/cool/checkpoints/session-1/9-call-1");

    // The checkpoint commit exists and HEAD/user index are untouched.
    git(dir.path(), &["rev-parse", "--verify", &reference]);
    assert_eq!(git(dir.path(), &["rev-parse", "HEAD"]), head_before);
    assert_eq!(git(dir.path(), &["ls-files", "--stage"]), index_before);
    assert!(git(dir.path(), &["status", "--porcelain"]).contains("A  a.txt"));
}

#[tokio::test]
async fn git_snapshot_and_restore_roundtrip() {
    let dir = git_repo();
    std::fs::write(dir.path().join("a.txt"), "v1").unwrap();
    let context = context(dir.path());
    let reference = snapshot_before_tool(
        &context,
        "write_file",
        "call-2",
        5,
        &args(serde_json::Map::from_iter([(
            "path".to_owned(),
            json!("a.txt"),
        )])),
    )
    .await
    .unwrap()
    .expect("checkpoint ref");

    // The tool mutates the workspace; the user also makes an unrelated file.
    std::fs::write(dir.path().join("a.txt"), "v2").unwrap();
    std::fs::write(dir.path().join("created.txt"), "new").unwrap();
    std::fs::write(dir.path().join("user-notes.txt"), "keep me").unwrap();

    restore_checkpoint(
        &context.workspace,
        &context.launcher,
        &context.environment,
        &reference,
        &["created.txt".to_owned()],
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
        "v1"
    );
    // The discarded tool's file is removed; an unrelated untracked file the
    // tool never named survives the restore.
    assert!(!dir.path().join("created.txt").exists());
    assert_eq!(
        std::fs::read_to_string(dir.path().join("user-notes.txt")).unwrap(),
        "keep me"
    );
}

#[tokio::test]
async fn manifest_fallback_restores_tool_known_files() {
    let dir = tempdir().unwrap(); // no .git — manifest fallback
    std::fs::write(dir.path().join("a.txt"), "v1").unwrap();
    let context = context(dir.path());

    let reference = snapshot_before_tool(
        &context,
        "edit_file",
        "call-3",
        2,
        &args(serde_json::Map::from_iter([(
            "path".to_owned(),
            json!("a.txt"),
        )])),
    )
    .await
    .unwrap()
    .expect("checkpoint ref");
    assert!(reference.starts_with("manifest:"), "{reference}");

    std::fs::write(dir.path().join("a.txt"), "v2").unwrap();
    restore_checkpoint(
        &context.workspace,
        &context.launcher,
        &context.environment,
        &reference,
        &[],
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
        "v1"
    );

    // A file created by the call (not present at snapshot time) is removed.
    let reference = snapshot_before_tool(
        &context,
        "write_file",
        "call-4",
        3,
        &args(serde_json::Map::from_iter([(
            "path".to_owned(),
            json!("new.txt"),
        )])),
    )
    .await
    .unwrap()
    .expect("checkpoint ref");
    std::fs::write(dir.path().join("new.txt"), "new").unwrap();
    restore_checkpoint(
        &context.workspace,
        &context.launcher,
        &context.environment,
        &reference,
        &[],
    )
    .await
    .unwrap();
    assert!(!dir.path().join("new.txt").exists());
}

#[tokio::test]
async fn shell_manifest_is_recorded_but_not_restorable() {
    let dir = tempdir().unwrap();
    let context = context(dir.path());
    let reference = snapshot_before_tool(
        &context,
        "shell",
        "call-5",
        7,
        &args(serde_json::Map::from_iter([(
            "command".to_owned(),
            json!("echo hi"),
        )])),
    )
    .await
    .unwrap()
    .expect("checkpoint ref");
    let error = restore_checkpoint(
        &context.workspace,
        &context.launcher,
        &context.environment,
        &reference,
        &[],
    )
    .await
    .unwrap_err();
    assert!(error.contains("not restorable"), "{error}");
}

#[tokio::test]
async fn disabled_launcher_and_untracked_tools_skip_the_snapshot() {
    let dir = git_repo();
    let disabled = ToolContext::new(
        Workspace::new(dir.path()).unwrap(),
        CapabilityPolicy::new(Some(Decision::Allow)),
    )
    .with_launcher(Arc::new(DisabledLauncher));
    assert!(
        snapshot_before_tool(&disabled, "write_file", "call-6", 1, &args(Map::new()))
            .await
            .unwrap()
            .is_none()
    );
    assert!(!tracks_tool("read_file"));
    assert!(tracks_tool("write_file"));

    let host = context(dir.path());
    assert!(
        snapshot_before_tool(&host, "read_file", "call-7", 1, &args(Map::new()))
            .await
            .unwrap()
            .is_none()
    );
}
