use std::io::Write;
use std::process::{Command, Stdio};

use serde_json::Value;

fn cool() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cool"))
}

fn write_plugin_fixture(root: &std::path::Path) {
    std::fs::create_dir_all(root.join("skills/demo")).unwrap();
    std::fs::write(
        root.join("plugin.json"),
        r#"{"$schema":"https://agent-plugins.org/schemas/1.0.0/plugin.schema.json","name":"cli-demo","version":"1"}"#,
    )
    .unwrap();
    std::fs::write(
        root.join("skills/demo/SKILL.md"),
        "---\nname: demo\ndescription: Demo skill\n---\nDo the thing.\n",
    )
    .unwrap();
}

#[test]
fn doctor_reports_the_m9_runtime_boundary() {
    let output = cool().arg("doctor").output().expect("run cool doctor");
    assert!(output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout).expect("doctor JSON");
    assert_eq!(report["status"], "ok");
    assert_eq!(report["phase"], "M9");
    assert_eq!(report["durableState"], true);
    assert_eq!(report["securityKernel"], true);
    assert_eq!(report["agentLoop"], true);
    assert_eq!(report["trustedTools"], true);
    assert_eq!(report["plugins"], true);
    assert_eq!(report["hooks"], true);
    assert_eq!(report["tui"], true);
    assert_eq!(report["acp"], true);
    assert!(report["capabilities"].as_array().unwrap().len() >= 5);
    assert!(
        report["capabilities"]
            .as_array()
            .unwrap()
            .iter()
            .any(|capability| capability == "session_fork")
    );
}

#[test]
fn plugin_lifecycle_commands_cover_install_list_validate_and_doctor() {
    let temporary = tempfile::tempdir().unwrap();
    let data_dir = temporary.path().join("data");
    let source = temporary.path().join("plugin-source");
    write_plugin_fixture(&source);

    let validate = cool()
        .env("COOL_DATA_DIR", &data_dir)
        .args(["plugin", "validate"])
        .arg(&source)
        .output()
        .expect("validate plugin");
    assert!(
        validate.status.success(),
        "{}",
        String::from_utf8_lossy(&validate.stderr)
    );
    let report: Value = serde_json::from_slice(&validate.stdout).unwrap();
    assert_eq!(report["name"], "cli-demo");
    assert_eq!(report["loadable"], true);
    assert_eq!(report["conformant"], true);

    let install = cool()
        .env("COOL_DATA_DIR", &data_dir)
        .args(["plugin", "install"])
        .arg(&source)
        .output()
        .expect("install plugin");
    assert!(
        install.status.success(),
        "{}",
        String::from_utf8_lossy(&install.stderr)
    );
    let installed: Value = serde_json::from_slice(&install.stdout).unwrap();
    assert_eq!(installed["name"], "cli-demo");
    assert_eq!(installed["enabled"], false);
    let lockfile = data_dir.join("plugins/plugins.lock.json");
    assert!(lockfile.is_file(), "the M3 lockfile is written");

    let list = cool()
        .env("COOL_DATA_DIR", &data_dir)
        .args(["plugin", "list"])
        .output()
        .expect("list plugins");
    assert!(list.status.success());
    let listed: Value = serde_json::from_slice(&list.stdout).unwrap();
    assert_eq!(listed["plugins"][0]["name"], "cli-demo");

    let doctor = cool()
        .env("COOL_DATA_DIR", &data_dir)
        .args(["plugin", "doctor"])
        .output()
        .expect("doctor plugins");
    assert!(doctor.status.success());
    let doctored: Value = serde_json::from_slice(&doctor.stdout).unwrap();
    assert_eq!(doctored["plugins"][0]["name"], "cli-demo");
    assert_eq!(doctored["plugins"][0]["loadable"], true);

    let missing = cool()
        .env("COOL_DATA_DIR", &data_dir)
        .args(["plugin", "install"])
        .arg(temporary.path().join("missing"))
        .output()
        .expect("install missing plugin");
    assert_eq!(missing.status.code(), Some(1));
    let error: Value = serde_json::from_slice(&missing.stderr).unwrap();
    assert_eq!(error["coolCode"], "plugin_install_failed");

    let mcp = cool()
        .env("COOL_DATA_DIR", &data_dir)
        .args(["mcp", "list"])
        .output()
        .expect("mcp list");
    assert!(mcp.status.success());
    let servers: Value = serde_json::from_slice(&mcp.stdout).unwrap();
    assert!(servers["servers"].as_array().unwrap().is_empty());

    let hooks = cool()
        .env("COOL_DATA_DIR", &data_dir)
        .args(["hooks", "list"])
        .output()
        .expect("hooks list");
    assert!(hooks.status.success());
    let hooks_json: Value = serde_json::from_slice(&hooks.stdout).unwrap();
    assert!(hooks_json["hooks"].as_array().unwrap().is_empty());
}

#[test]
fn acp_stdio_subprocess_negotiates_acp_v1() {
    let temporary = tempfile::tempdir().unwrap();
    let mut child = cool()
        .env("COOL_DATA_DIR", temporary.path().join("data"))
        .arg("acp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn cool acp");
    let mut stdin = child.stdin.take().unwrap();
    stdin
        .write_all(
            br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1,"clientCapabilities":{}}}
"#,
        )
        .unwrap();
    stdin.flush().unwrap();
    drop(stdin);

    let output = child.wait_with_output().expect("cool acp exits on EOF");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let frame: Value = serde_json::from_str(stdout.lines().next().expect("one ACP frame"))
        .expect("ACP initialize response");
    assert_eq!(frame["id"], 1);
    assert_eq!(frame["result"]["protocolVersion"], 1);
    assert_eq!(frame["result"]["agentCapabilities"]["loadSession"], true);
}

#[test]
fn tui_command_is_routed_and_rejects_arguments_without_a_terminal() {
    let output = cool()
        .args(["tui", "unexpected"])
        .output()
        .expect("run tui with arguments");
    assert_eq!(output.status.code(), Some(2));
    let error: Value = serde_json::from_slice(&output.stderr).expect("structured CLI error");
    assert_eq!(error["coolCode"], "invalid_cli_usage");
}

#[test]
fn tui_fails_closed_without_an_interactive_terminal() {
    let output = cool().output().expect("run cool without a terminal");
    assert_eq!(output.status.code(), Some(1));
    let error: Value = serde_json::from_slice(&output.stderr).expect("structured CLI error");
    assert_eq!(error["coolCode"], "tui_requires_terminal");
    assert_eq!(error["retryable"], false);
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
