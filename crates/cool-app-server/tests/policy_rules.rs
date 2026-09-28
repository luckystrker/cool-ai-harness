use std::sync::Arc;

use cool_agent::{AgentRuntime, ScriptedDriver, builtin_registry};
use cool_app_server::client::new_idempotency_key;
use cool_app_server::{AppClient, AppServer, ServerConfig};
use cool_protocol::{
    ApprovalResolveParams, Command, IdempotencyKey, PolicyRuleAddParams, PolicyRuleDeleteParams,
    PolicyRuleRecord, PolicyRulesListParams, ResponsePayload,
};
use cool_security::{CapabilityPolicy, Decision, Workspace};
use cool_state::DurableStore;
use serde_json::json;
use tempfile::tempdir;

fn key(prefix: &str) -> IdempotencyKey {
    IdempotencyKey::new(new_idempotency_key(prefix)).unwrap()
}

async fn connected_client(
    server: AppServer,
) -> (AppClient, tokio::task::JoinHandle<std::io::Result<()>>) {
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    let (reader, writer) = tokio::io::split(client_io);
    let task = tokio::spawn(async move { server.serve_io(server_io).await });
    let client = AppClient::connect(reader, writer).expect("client connects");
    client.initialize("policy-rules-test", "1").await.unwrap();
    (client, task)
}

fn server(workspace: &std::path::Path, with_legacy: bool) -> AppServer {
    server_with_driver(workspace, with_legacy, Arc::new(ScriptedDriver::new([])))
}

fn server_with_driver(
    workspace: &std::path::Path,
    with_legacy: bool,
    driver: Arc<ScriptedDriver>,
) -> AppServer {
    let mut config = ServerConfig::default();
    if with_legacy {
        config.legacy_store = Some(Arc::new(cool_store::LegacyStore::in_memory().unwrap()));
    }
    AppServer::with_agent_runtime(
        config,
        DurableStore::in_memory().unwrap(),
        AgentRuntime::new(driver, builtin_registry()),
        Workspace::new(workspace).unwrap(),
        CapabilityPolicy::new(Some(Decision::Allow)),
        "scripted",
    )
    .unwrap()
}

fn shell_rule(scope: &str) -> PolicyRuleRecord {
    PolicyRuleRecord {
        id: None,
        tool: "shell".to_owned(),
        kind: "command".to_owned(),
        pattern: "cargo *".to_owned(),
        decision: "allow".to_owned(),
        scope: scope.to_owned(),
        note: None,
    }
}

async fn rules_list(
    client: &AppClient,
    scope: Option<&str>,
    run_id: Option<&str>,
) -> Vec<PolicyRuleRecord> {
    match client
        .request(Command::PolicyRulesList(PolicyRulesListParams {
            scope: scope.map(str::to_owned),
            run_id: run_id.map(str::to_owned),
        }))
        .await
        .unwrap()
    {
        ResponsePayload::PolicyRulesListed(result) => result.rules,
        payload => panic!("unexpected rules_list payload: {payload:?}"),
    }
}

#[tokio::test]
async fn project_rules_persist_to_workspace_policy_json_and_list() {
    let directory = tempdir().unwrap();
    let app = server(directory.path(), false);
    let (client, task) = connected_client(app.clone()).await;

    let added = client
        .request(Command::PolicyRuleAdd(PolicyRuleAddParams {
            rule: shell_rule("project"),
            run_id: None,
        }))
        .await
        .unwrap();
    let ResponsePayload::PolicyRuleAdded(rule) = added else {
        panic!("unexpected rule_add payload: {added:?}")
    };
    assert_eq!(rule.id.as_deref(), Some("project:0"));
    assert_eq!(rule.scope, "project");

    // The rule is durable on disk, not only in memory.
    let policy_json = directory.path().join(".cool").join("policy.json");
    let persisted: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&policy_json).unwrap()).unwrap();
    let rules = persisted["rules"].as_array().expect("rules array");
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0]["pattern"], json!("cargo *"));

    let listed = rules_list(&client, Some("project"), None).await;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].tool, "shell");
    assert_eq!(listed[0].decision, "allow");

    // Deleting removes both the in-memory rule and the persisted file entry.
    match client
        .request(Command::PolicyRuleDelete(PolicyRuleDeleteParams {
            idempotency_key: key("rule-delete"),
            rule_id: "project:0".to_owned(),
            run_id: None,
        }))
        .await
        .unwrap()
    {
        ResponsePayload::PolicyRuleDeleted(result) => assert!(result.deleted),
        payload => panic!("unexpected rule_delete payload: {payload:?}"),
    }
    assert!(rules_list(&client, Some("project"), None).await.is_empty());

    drop(client);
    task.await.expect("server task").expect("clean disconnect");
}

#[tokio::test]
async fn user_rules_persist_in_the_durable_store() {
    let directory = tempdir().unwrap();
    let app = server(directory.path(), true);
    let (client, task) = connected_client(app.clone()).await;

    let added = client
        .request(Command::PolicyRuleAdd(PolicyRuleAddParams {
            rule: shell_rule("user"),
            run_id: None,
        }))
        .await
        .unwrap();
    let ResponsePayload::PolicyRuleAdded(rule) = added else {
        panic!("unexpected rule_add payload: {added:?}")
    };
    assert!(rule.id.as_deref().unwrap().starts_with("user:"));

    let listed = rules_list(&client, Some("user"), None).await;
    assert_eq!(listed.len(), 1);

    // A second server over the same durable store sees the rule (persistence
    // is on the store, not on the process).
    drop(client);
    task.await.expect("server task").expect("clean disconnect");
}

#[tokio::test]
async fn session_rules_attach_to_their_run_and_delete() {
    let directory = tempdir().unwrap();
    let app = server(directory.path(), false);
    let (client, task) = connected_client(app.clone()).await;

    let added = client
        .request(Command::PolicyRuleAdd(PolicyRuleAddParams {
            rule: shell_rule("session"),
            run_id: Some("run-1".to_owned()),
        }))
        .await
        .unwrap();
    let ResponsePayload::PolicyRuleAdded(rule) = added else {
        panic!("unexpected rule_add payload: {added:?}")
    };
    assert_eq!(rule.id.as_deref(), Some("session:0"));

    assert_eq!(
        rules_list(&client, Some("session"), Some("run-1"))
            .await
            .len(),
        1
    );
    assert!(
        rules_list(&client, Some("session"), Some("run-2"))
            .await
            .is_empty()
    );

    match client
        .request(Command::PolicyRuleDelete(PolicyRuleDeleteParams {
            idempotency_key: key("rule-delete-session"),
            rule_id: "session:0".to_owned(),
            run_id: Some("run-1".to_owned()),
        }))
        .await
        .unwrap()
    {
        ResponsePayload::PolicyRuleDeleted(result) => assert!(result.deleted),
        payload => panic!("unexpected rule_delete payload: {payload:?}"),
    }

    drop(client);
    task.await.expect("server task").expect("clean disconnect");
}

#[tokio::test]
async fn approval_resolve_with_remember_persists_and_lists_the_rule() {
    let directory = tempdir().unwrap();
    // The run must stay alive while the approval ticket is created; the
    // delayed echo keeps it from finishing before the resolve lands.
    let app = server_with_driver(
        directory.path(),
        true,
        Arc::new(ScriptedDriver::echo_with_delay(
            std::time::Duration::from_secs(10),
        )),
    );
    let (client, task) = connected_client(app.clone()).await;
    let session_id = match client
        .request(Command::SessionCreate(cool_protocol::SessionCreateParams {
            idempotency_key: key("session"),
            title: Some("rules".to_owned()),
            project_key: None,
        }))
        .await
        .unwrap()
    {
        ResponsePayload::SessionCreated(result) => result.session_id,
        payload => panic!("unexpected session payload: {payload:?}"),
    };
    let run_id = match client
        .request(Command::SessionPrompt(cool_protocol::SessionPromptParams {
            idempotency_key: key("prompt"),
            session_id: session_id.clone(),
            content: vec![cool_protocol::ContentPart::Text {
                text: "hi".to_owned(),
            }],
            model: None,
            plan_mode: false,
            system_prompt: None,
            long_task_mode: false,
        }))
        .await
        .unwrap()
    {
        ResponsePayload::PromptAccepted(result) => result.run_id,
        payload => panic!("unexpected prompt payload: {payload:?}"),
    };
    // The stored call is `cargo run` — the remembered rule must cover it.
    let ticket = app
        .create_approval(
            &session_id,
            &run_id,
            "call-1",
            "shell",
            &std::collections::BTreeMap::from([
                ("program".to_owned(), json!("cargo")),
                ("args".to_owned(), json!(["run"])),
            ]),
            "run cargo",
        )
        .unwrap();

    match client
        .request(Command::ApprovalResolve(ApprovalResolveParams {
            idempotency_key: key("resolve"),
            approval_id: ticket.approval_id.clone(),
            expected_revision: ticket.revision,
            decision: cool_protocol::ApprovalDecision::Approved,
            remember: Some("user".to_owned()),
            rule: Some(shell_rule("user")),
        }))
        .await
        .unwrap()
    {
        ResponsePayload::ApprovalResolved(result) => {
            let remembered = result.remembered_rule.expect("remembered rule is reported");
            assert!(remembered.id.as_deref().unwrap().starts_with("user:"));
            assert_eq!(remembered.pattern, "cargo *");
        }
        payload => panic!("unexpected resolve payload: {payload:?}"),
    }

    // The persisted user rule is now listed and merged into future policies.
    let listed = rules_list(&client, Some("user"), None).await;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].tool, "shell");

    drop(client);
    task.await.expect("server task").expect("clean disconnect");
}

/// A `remember` rule that does not cover the approved call must be rejected:
/// an approval for one tool must not mint an allow rule for another (SEC).
#[tokio::test]
async fn approval_resolve_rejects_rule_unrelated_to_the_call() {
    let directory = tempdir().unwrap();
    let app = server_with_driver(
        directory.path(),
        true,
        Arc::new(ScriptedDriver::echo_with_delay(
            std::time::Duration::from_secs(10),
        )),
    );
    let (client, task) = connected_client(app.clone()).await;
    let session_id = match client
        .request(Command::SessionCreate(cool_protocol::SessionCreateParams {
            idempotency_key: key("session"),
            title: Some("rules".to_owned()),
            project_key: None,
        }))
        .await
        .unwrap()
    {
        ResponsePayload::SessionCreated(result) => result.session_id,
        payload => panic!("unexpected session payload: {payload:?}"),
    };
    let run_id = match client
        .request(Command::SessionPrompt(cool_protocol::SessionPromptParams {
            idempotency_key: key("prompt"),
            session_id: session_id.clone(),
            content: vec![cool_protocol::ContentPart::Text {
                text: "hi".to_owned(),
            }],
            model: None,
            plan_mode: false,
            system_prompt: None,
            long_task_mode: false,
        }))
        .await
        .unwrap()
    {
        ResponsePayload::PromptAccepted(result) => result.run_id,
        payload => panic!("unexpected prompt payload: {payload:?}"),
    };
    let ticket = app
        .create_approval(
            &session_id,
            &run_id,
            "call-1",
            "shell",
            &std::collections::BTreeMap::from([
                ("program".to_owned(), json!("cargo")),
                ("args".to_owned(), json!(["run"])),
            ]),
            "run cargo",
        )
        .unwrap();

    // `rm *` does not cover `cargo run` — rejected, nothing persisted.
    let mut unrelated = shell_rule("user");
    unrelated.pattern = "rm *".to_owned();
    match client
        .request(Command::ApprovalResolve(ApprovalResolveParams {
            idempotency_key: key("resolve-unrelated"),
            approval_id: ticket.approval_id.clone(),
            expected_revision: ticket.revision,
            decision: cool_protocol::ApprovalDecision::Approved,
            remember: Some("user".to_owned()),
            rule: Some(unrelated),
        }))
        .await
    {
        Err(error) => assert!(
            error.to_string().contains("rule_persist_failed")
                || error.to_string().contains("does not cover"),
            "unexpected error: {error}"
        ),
        Ok(payload) => panic!("unrelated rule must not persist: {payload:?}"),
    }
    assert!(rules_list(&client, Some("user"), None).await.is_empty());

    // Replays dedupe: the same key resolves again and still one rule exists.
    let resolve_key = key("resolve");
    match client
        .request(Command::ApprovalResolve(ApprovalResolveParams {
            idempotency_key: resolve_key.clone(),
            approval_id: ticket.approval_id.clone(),
            expected_revision: ticket.revision,
            decision: cool_protocol::ApprovalDecision::Approved,
            remember: Some("user".to_owned()),
            rule: Some(shell_rule("user")),
        }))
        .await
        .unwrap()
    {
        ResponsePayload::ApprovalResolved(result) => {
            assert!(result.remembered_rule.is_some());
        }
        payload => panic!("unexpected resolve payload: {payload:?}"),
    }
    client
        .request(Command::ApprovalResolve(ApprovalResolveParams {
            idempotency_key: resolve_key,
            approval_id: ticket.approval_id.clone(),
            expected_revision: ticket.revision,
            decision: cool_protocol::ApprovalDecision::Approved,
            remember: Some("user".to_owned()),
            rule: Some(shell_rule("user")),
        }))
        .await
        .unwrap();
    assert_eq!(rules_list(&client, Some("user"), None).await.len(), 1);

    drop(client);
    task.await.expect("server task").expect("clean disconnect");
}

/// Project-rule ids are stable across deletes — deleting `project:0` never
/// reassigns `project:1`, and the next add gets `project:2` (SEC: a stale id
/// must never delete the wrong rule).
#[tokio::test]
async fn project_rule_ids_are_stable_across_deletes() {
    let directory = tempdir().unwrap();
    let app = server(directory.path(), false);
    let (client, task) = connected_client(app.clone()).await;

    for pattern in ["cargo *", "npm *"] {
        let mut rule = shell_rule("project");
        rule.pattern = pattern.to_owned();
        client
            .request(Command::PolicyRuleAdd(PolicyRuleAddParams {
                rule,
                run_id: None,
            }))
            .await
            .unwrap();
    }
    client
        .request(Command::PolicyRuleDelete(PolicyRuleDeleteParams {
            idempotency_key: key("rule-delete-0"),
            rule_id: "project:0".to_owned(),
            run_id: None,
        }))
        .await
        .unwrap();
    let mut third = shell_rule("project");
    third.pattern = "pip *".to_owned();
    match client
        .request(Command::PolicyRuleAdd(PolicyRuleAddParams {
            rule: third,
            run_id: None,
        }))
        .await
        .unwrap()
    {
        ResponsePayload::PolicyRuleAdded(rule) => {
            assert_eq!(rule.id.as_deref(), Some("project:2"));
        }
        payload => panic!("unexpected rule_add payload: {payload:?}"),
    }
    // `project:1` still addresses the npm rule — deleting it removes npm.
    client
        .request(Command::PolicyRuleDelete(PolicyRuleDeleteParams {
            idempotency_key: key("rule-delete-1"),
            rule_id: "project:1".to_owned(),
            run_id: None,
        }))
        .await
        .unwrap();
    let listed = rules_list(&client, Some("project"), None).await;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].pattern, "pip *");

    drop(client);
    task.await.expect("server task").expect("clean disconnect");
}

/// A malformed `.cool/policy.json` fails closed: the server loads a deny-all
/// rule instead of silently falling back to the permissive policy (SEC).
#[tokio::test]
async fn malformed_policy_json_loads_a_deny_all_rule() {
    let directory = tempdir().unwrap();
    std::fs::create_dir_all(directory.path().join(".cool")).unwrap();
    std::fs::write(
        directory.path().join(".cool/policy.json"),
        "{ not valid json !!!",
    )
    .unwrap();
    let app = server(directory.path(), false);
    let (client, task) = connected_client(app.clone()).await;

    let listed = rules_list(&client, Some("project"), None).await;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].tool, "*");
    assert_eq!(listed[0].decision, "deny");
    assert!(
        listed[0]
            .note
            .as_deref()
            .unwrap_or_default()
            .contains("malformed")
    );

    drop(client);
    task.await.expect("server task").expect("clean disconnect");
}
