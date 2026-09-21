use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

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
fn doctor_reports_the_m11_runtime_boundary() {
    let temporary = tempfile::tempdir().unwrap();
    let output = cool()
        .arg("doctor")
        .arg("--data-dir")
        .arg(temporary.path())
        .output()
        .expect("run cool doctor");
    assert!(output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout).expect("doctor JSON");
    assert_eq!(report["status"], "ok");
    assert_eq!(report["phase"], "M11");
    assert_eq!(report["webFacade"], true);
    assert_eq!(report["serveProfiles"], json!(["local", "server"]));
    assert_eq!(report["durableState"], true);
    assert_eq!(report["securityKernel"], true);
    assert_eq!(report["agentLoop"], true);
    assert_eq!(report["trustedTools"], true);
    assert_eq!(report["plugins"], true);
    assert_eq!(report["hooks"], true);
    assert_eq!(report["tui"], true);
    assert_eq!(report["acp"], true);
    // An empty data directory has no legacy store yet; the doctor reports that
    // instead of adopting or writing anything.
    assert_eq!(report["legacyStore"]["status"], "absent");
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
fn app_server_legacy_store_flag_reads_python_data_without_adopting() {
    let temporary = tempfile::tempdir().unwrap();
    let data_dir = temporary.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let database = data_dir.join("harness.db");
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .execute_batch(cool_store::BASELINE_SCHEMA_SQL)
        .unwrap();
    connection
        .execute_batch(
            "INSERT INTO users(created_at, updated_at, id, external_id, username, display_name, is_active)
             VALUES ('2026-01-01 00:00:00.000000', '2026-01-01 00:00:00.000000', 1, 'local', 'local', 'Local', 1);
             INSERT INTO conversations(created_at, updated_at, id, user_id, title, is_pinned, is_archived)
             VALUES ('2026-01-01 00:00:00.000000', '2026-01-01 00:00:00.000000', 1, 1, 'Legacy chat', 0, 0);",
        )
        .unwrap();
    drop(connection);

    let mut child = cool()
        .args(["app-server", "--data-dir"])
        .arg(&data_dir)
        .arg("--legacy-store")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    writeln!(
        stdin,
        "{}",
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "cool.command",
            "params": {
                "protocolVersion": 1,
                "commandId": "cli-init",
                "command": {
                    "method": "initialize",
                    "params": {
                        "clientName": "cli-test",
                        "clientVersion": "1",
                        "supportedProtocolVersions": [1],
                        "capabilities": []
                    }
                }
            }
        })
    )
    .unwrap();
    writeln!(
        stdin,
        "{}",
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "cool.command",
            "params": {
                "protocolVersion": 1,
                "commandId": "cli-list",
                "command": {
                    "method": "conversations.list",
                    "params": {
                        "includeMachineOwned": false,
                        "archived": null,
                        "pinned": null,
                        "folder": null,
                        "search": null,
                        "limit": 10,
                        "offset": 0
                    }
                }
            }
        })
    )
    .unwrap();
    stdin.flush().unwrap();

    let reader = BufReader::new(child.stdout.take().unwrap());
    let mut lines = reader.lines();
    let initialized: Value =
        serde_json::from_str(&lines.next().expect("initialize response").unwrap()).unwrap();
    assert_eq!(initialized["result"]["kind"], "initialized");
    let listed: Value =
        serde_json::from_str(&lines.next().expect("list response").unwrap()).unwrap();
    let conversations = listed["result"]["value"].as_array().unwrap();
    assert_eq!(conversations.len(), 1);
    assert_eq!(conversations[0]["title"], "Legacy chat");

    drop(stdin);
    let _ = child.wait();

    let reopened = cool_store::LegacyStore::open_read_only(&database).unwrap();
    assert!(
        reopened.meta().unwrap().owner.is_none() && !reopened.is_rust_owned().unwrap(),
        "the explicit flag must not adopt a Python-owned store"
    );
    assert_eq!(
        reopened.alembic_revision().unwrap().as_deref(),
        Some("0022")
    );
}

/// Seeds `harness.db` with the baseline schema and one legacy conversation.
fn seed_legacy_conversation(database: &std::path::Path, title: &str) {
    let connection = rusqlite::Connection::open(database).unwrap();
    connection
        .execute_batch(cool_store::BASELINE_SCHEMA_SQL)
        .unwrap();
    connection
        .execute(
            "INSERT INTO users(created_at, updated_at, id, external_id, username, display_name, is_active)
             VALUES ('2026-01-01 00:00:00.000000', '2026-01-01 00:00:00.000000', 1, 'local', 'local', 'Local', 1)",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO conversations(created_at, updated_at, id, user_id, title, is_pinned, is_archived)
             VALUES ('2026-01-01 00:00:00.000000', '2026-01-01 00:00:00.000000', 1, 1, ?1, 0, 0)",
            rusqlite::params![title],
        )
        .unwrap();
}

/// Runs `cool app-server --legacy-store` over a data dir and returns the
/// conversation titles the canonical `conversations.list` family reports.
fn legacy_conversation_titles(data_dir: &std::path::Path) -> Vec<String> {
    let mut child = cool()
        .args(["app-server", "--data-dir"])
        .arg(data_dir)
        .arg("--legacy-store")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    writeln!(
        stdin,
        "{}",
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "cool.command",
            "params": {
                "protocolVersion": 1,
                "commandId": "upgrade-init",
                "command": {
                    "method": "initialize",
                    "params": {
                        "clientName": "cli-test",
                        "clientVersion": "1",
                        "supportedProtocolVersions": [1],
                        "capabilities": []
                    }
                }
            }
        })
    )
    .unwrap();
    writeln!(
        stdin,
        "{}",
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "cool.command",
            "params": {
                "protocolVersion": 1,
                "commandId": "upgrade-list",
                "command": {
                    "method": "conversations.list",
                    "params": {
                        "includeMachineOwned": false,
                        "archived": null,
                        "pinned": null,
                        "folder": null,
                        "search": null,
                        "limit": 10,
                        "offset": 0
                    }
                }
            }
        })
    )
    .unwrap();
    stdin.flush().unwrap();

    let reader = BufReader::new(child.stdout.take().unwrap());
    let mut lines = reader.lines();
    let _initialized = lines.next().expect("initialize response").unwrap();
    let listed: Value =
        serde_json::from_str(&lines.next().expect("list response").unwrap()).unwrap();
    let titles = listed["result"]["value"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|conversation| conversation["title"].as_str().map(str::to_owned))
        .collect();
    drop(stdin);
    let _ = child.wait();
    titles
}

#[test]
fn store_adopt_backs_up_serves_and_restores_legacy_data() {
    let temporary = tempfile::tempdir().unwrap();
    let data_dir = temporary.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let database = data_dir.join("harness.db");
    seed_legacy_conversation(&database, "Upgrade chat");

    // Before adoption the doctor reports a Python-owned store and the explicit
    // legacy flag reads it without writing.
    let doctor = cool()
        .arg("doctor")
        .arg("--data-dir")
        .arg(&data_dir)
        .output()
        .unwrap();
    assert!(doctor.status.success());
    let report: Value = serde_json::from_slice(&doctor.stdout).unwrap();
    assert_eq!(report["legacyStore"]["status"], "python-owned");

    // Adoption is an explicit operator action: it takes a verified backup and
    // records the Rust migration owner.
    let adopt = cool()
        .args(["store", "adopt", "--data-dir"])
        .arg(&data_dir)
        .output()
        .unwrap();
    assert!(
        adopt.status.success(),
        "adopt failed: {}",
        String::from_utf8_lossy(&adopt.stderr)
    );
    let adopted: Value = serde_json::from_slice(&adopt.stdout).expect("adopt JSON");
    assert_eq!(adopted["status"], "ok");
    assert_eq!(adopted["adopted"], true);
    assert_eq!(adopted["alembicRevision"], "0022");
    assert_eq!(adopted["rustOwned"], true);
    let backup = std::path::PathBuf::from(adopted["backupPath"].as_str().expect("backup path"));
    assert!(backup.is_file(), "adoption must leave a verified backup");

    let store = cool_store::LegacyStore::open_read_only(&database).unwrap();
    assert!(store.is_rust_owned().unwrap());
    assert_eq!(store.alembic_revision().unwrap().as_deref(), Some("0022"));
    drop(store);

    // The new binary now serves the adopted store: the legacy row survives and
    // a fresh rust-core.db is created next to it.
    assert_eq!(
        legacy_conversation_titles(&data_dir),
        vec!["Upgrade chat".to_owned()]
    );
    assert!(data_dir.join("rust-core.db").is_file());

    // Re-adoption is idempotent and takes no new backup.
    let again = cool()
        .args(["store", "adopt", "--data-dir"])
        .arg(&data_dir)
        .output()
        .unwrap();
    assert!(again.status.success());
    let again: Value = serde_json::from_slice(&again.stdout).unwrap();
    assert_eq!(again["adopted"], false);
    assert!(again["backupPath"].is_null());

    // Rollback restores the pre-adoption snapshot: the store is no longer
    // Rust-owned and the conversation is intact.
    cool_store::restore_backup(&backup, &database).expect("restore backup");
    let rolled_back = cool_store::LegacyStore::open_read_only(&database).unwrap();
    assert!(!rolled_back.is_rust_owned().unwrap());
    assert_eq!(
        rolled_back.alembic_revision().unwrap().as_deref(),
        Some("0022")
    );
    drop(rolled_back);
    assert_eq!(
        legacy_conversation_titles(&data_dir),
        vec!["Upgrade chat".to_owned()]
    );
}

#[test]
fn app_server_legacy_store_initializes_a_fresh_baseline_without_python() {
    // A fresh data root has no Python store; the explicit entrypoint initializes
    // the baseline and takes Rust ownership so the legacy families are served.
    let temporary = tempfile::tempdir().unwrap();
    let data_dir = temporary.path().join("fresh");
    std::fs::create_dir_all(&data_dir).unwrap();
    assert!(
        legacy_conversation_titles(&data_dir).is_empty(),
        "a fresh baseline has no conversations"
    );
    let database = data_dir.join("harness.db");
    let store = cool_store::LegacyStore::open_read_only(&database).unwrap();
    assert!(store.is_rust_owned().unwrap());
    assert_eq!(store.alembic_revision().unwrap().as_deref(), Some("0022"));
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
fn serve_server_profile_fails_closed_without_a_token() {
    let temporary = tempfile::tempdir().unwrap();
    let output = cool()
        .args([
            "serve",
            "--data-dir",
            temporary.path().to_str().unwrap(),
            "--profile",
            "server",
            "--port",
            "0",
        ])
        .output()
        .expect("run cool serve server profile");
    assert_eq!(output.status.code(), Some(2));
    let error: Value = serde_json::from_slice(&output.stderr).expect("structured CLI error");
    assert_eq!(error["coolCode"], "invalid_cli_usage");
    assert_eq!(error["retryable"], false);
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("requires an API token"),
        "message: {}",
        error["message"]
    );
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

#[test]
fn serve_starts_the_http_facade_and_answers_health() {
    let temporary = tempfile::tempdir().unwrap();
    let mut child = cool()
        .args([
            "serve",
            "--data-dir",
            temporary.path().to_str().unwrap(),
            "--port",
            "0",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn cool serve");

    let stderr = child.stderr.take().expect("stderr pipe");
    let mut address = None;
    for line in BufReader::new(stderr).lines() {
        let line = line.expect("stderr line");
        if let Some(rest) = line.split("listening on http://").nth(1) {
            address = Some(rest.trim().to_owned());
            break;
        }
    }
    let address = address.expect("serve announced its address");

    let mut stream = std::net::TcpStream::connect(&address).expect("connect to serve");
    write!(
        stream,
        "GET /api/health HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"
    )
    .expect("write request");
    stream.flush().expect("flush request");
    let mut response = String::new();
    stream.read_to_string(&mut response).expect("read response");
    let _ = child.kill();
    let _ = child.wait();

    assert!(response.contains("200 OK"), "response:\n{response}");
    assert!(
        response.contains("\"runtime\":\"rust-trusted-core\""),
        "response:\n{response}"
    );
    assert!(
        response.contains("\"phase\":\"M11\""),
        "response:\n{response}"
    );
}
