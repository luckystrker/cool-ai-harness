use std::process::Command;

use serde_json::Value;

fn cool() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cool"))
}

#[test]
fn doctor_reports_the_m6_runtime_boundary() {
    let output = cool().arg("doctor").output().expect("run cool doctor");
    assert!(output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout).expect("doctor JSON");
    assert_eq!(report["status"], "ok");
    assert_eq!(report["phase"], "M6");
    assert_eq!(report["durableState"], true);
    assert_eq!(report["securityKernel"], true);
    assert_eq!(report["agentLoop"], false);
    assert!(report["capabilities"].as_array().unwrap().len() >= 5);
}

#[test]
fn later_phase_routes_fail_closed_with_structured_errors() {
    for route in ["serve", "run"] {
        let output = cool().arg(route).output().expect("run routed command");
        assert_eq!(output.status.code(), Some(2));
        let error: Value = serde_json::from_slice(&output.stderr).expect("structured CLI error");
        assert_eq!(error["coolCode"], "m7_route_not_implemented");
        assert_eq!(error["retryable"], false);
    }
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
