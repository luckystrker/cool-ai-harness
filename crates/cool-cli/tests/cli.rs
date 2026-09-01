use std::process::Command;

use serde_json::Value;

fn cool() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cool"))
}

#[test]
fn doctor_reports_the_m7_runtime_boundary() {
    let output = cool().arg("doctor").output().expect("run cool doctor");
    assert!(output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout).expect("doctor JSON");
    assert_eq!(report["status"], "ok");
    assert_eq!(report["phase"], "M7");
    assert_eq!(report["durableState"], true);
    assert_eq!(report["securityKernel"], true);
    assert_eq!(report["agentLoop"], true);
    assert_eq!(report["trustedTools"], true);
    assert!(report["capabilities"].as_array().unwrap().len() >= 5);
}

#[test]
fn later_phase_serve_route_fails_closed_with_structured_error() {
    let output = cool().arg("serve").output().expect("run routed command");
    assert_eq!(output.status.code(), Some(2));
    let error: Value = serde_json::from_slice(&output.stderr).expect("structured CLI error");
    assert_eq!(error["coolCode"], "m11_route_not_implemented");
    assert_eq!(error["retryable"], false);
}

#[test]
fn scripted_non_interactive_run_uses_the_rust_agent_loop() {
    let output = cool()
        .args(["run", "--scripted", "hello", "rust"])
        .output()
        .expect("run scripted agent");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "hello rust");
}

#[test]
fn non_scripted_run_fails_closed_without_provider_credentials() {
    let output = cool()
        .env_remove("OPENAI_API_KEY")
        .env_remove("OPENAI_BASE_URL")
        .args(["run", "hello"])
        .output()
        .expect("run without provider key");
    assert_eq!(output.status.code(), Some(1));
    let error: Value = serde_json::from_slice(&output.stderr).expect("structured provider error");
    assert_eq!(error["coolCode"], "provider_credentials_missing");
}

#[test]
fn invalid_transport_is_rejected_before_server_start() {
    let output = cool()
        .args(["app-server", "--transport", "tcp"])
        .output()
        .expect("run invalid app server command");
    assert_eq!(output.status.code(), Some(2));
    let error: Value = serde_json::from_slice(&output.stderr).expect("structured CLI error");
    assert_eq!(error["coolCode"], "invalid_cli_usage");
}
