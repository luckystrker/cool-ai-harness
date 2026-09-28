use std::sync::Arc;

use cool_agent::{HostLauncher, ToolContext, builtin_registry};
use cool_security::{CapabilityPolicy, Decision, Workspace};
use serde_json::{Value, json};
use tempfile::tempdir;

fn context(root: &std::path::Path) -> ToolContext {
    ToolContext::new(
        Workspace::new(root).unwrap(),
        CapabilityPolicy::new(Some(Decision::Allow)),
    )
}

fn write_config(root: &std::path::Path, extension: &str, argv: Value) {
    let directory = root.join(".cool");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join("config.json"),
        json!({"diagnostics": {extension: argv}}).to_string(),
    )
    .unwrap();
}

#[tokio::test]
async fn write_file_diagnostics_skipped_when_launcher_disabled() {
    let directory = tempdir().unwrap();
    // An extension nobody else configures keeps the user-level map from
    // interfering with the assertion.
    write_config(
        directory.path(),
        "cooldiagx",
        json!(["cmd", "/c", "echo", "diag:{file}"]),
    );
    let write = builtin_registry().get("write_file").unwrap();
    let result = write
        .execute(
            &context(directory.path()),
            json!({"path": "note.cooldiagx", "content": "x"}),
        )
        .await
        .unwrap();
    assert!(!result.is_error);
    assert_eq!(result.diagnostics.as_deref(), Some("skipped"));
}

#[tokio::test]
async fn write_file_diagnostics_absent_without_a_configured_extension() {
    let directory = tempdir().unwrap();
    let write = builtin_registry().get("write_file").unwrap();
    let result = write
        .execute(
            &context(directory.path()),
            json!({"path": "note.cooldiagnone", "content": "x"}),
        )
        .await
        .unwrap();
    assert!(!result.is_error);
    assert!(result.diagnostics.is_none());
}

#[tokio::test]
async fn write_file_diagnostics_run_through_the_configured_launcher() {
    let directory = tempdir().unwrap();
    #[cfg(windows)]
    let argv = json!([
        std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_owned()),
        "/c",
        "echo diag-ok:{file}"
    ]);
    #[cfg(not(windows))]
    let argv = json!(["/bin/sh", "-c", "echo diag-ok:{file}"]);
    write_config(directory.path(), "cooldiag", argv);
    let mut context = context(directory.path());
    context.launcher = Arc::new(HostLauncher);
    context.environment = std::env::vars().collect();
    let write = builtin_registry().get("write_file").unwrap();
    let result = write
        .execute(&context, json!({"path": "note.cooldiag", "content": "x"}))
        .await
        .unwrap();
    assert!(!result.is_error);
    let diagnostics = result.diagnostics.expect("diagnostics ran");
    assert!(
        diagnostics.contains("diag-ok:note.cooldiag"),
        "unexpected diagnostics output: {diagnostics:?}"
    );
}

#[tokio::test]
async fn edit_file_diagnostics_skipped_when_launcher_disabled() {
    let directory = tempdir().unwrap();
    write_config(
        directory.path(),
        "cooldiagy",
        json!(["cmd", "/c", "echo", "diag:{file}"]),
    );
    std::fs::write(directory.path().join("seed.cooldiagy"), "anchor").unwrap();
    let edit = builtin_registry().get("edit_file").unwrap();
    let result = edit
        .execute(
            &context(directory.path()),
            json!({
                "path": "seed.cooldiagy",
                "edits": [{"old": "anchor", "new": "edited"}],
            }),
        )
        .await
        .unwrap();
    assert!(!result.is_error);
    assert_eq!(result.diagnostics.as_deref(), Some("skipped"));
}
