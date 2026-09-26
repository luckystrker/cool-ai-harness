use std::collections::{BTreeSet, HashMap};
use std::time::Duration;

use cool_agent::{CancelSignal, ToolContext, ToolError, builtin_registry};
use cool_security::{Capability, CapabilityPolicy, Decision, Workspace};
use serde_json::json;
use tempfile::tempdir;

fn context(root: &std::path::Path) -> ToolContext {
    ToolContext::new(
        Workspace::new(root).unwrap(),
        CapabilityPolicy::new(Some(Decision::Allow)),
    )
}

#[tokio::test]
async fn file_tools_confine_reads_and_writes_to_the_workspace() {
    let directory = tempdir().unwrap();
    let registry = builtin_registry();
    let context = context(directory.path());
    let write = registry.get("write_file").unwrap();
    let denied = write
        .execute(&context, json!({"path":"../escape.txt","content":"no"}))
        .await
        .expect_err("parent escape must fail before I/O");
    assert!(denied.to_string().contains("security policy"));
    assert!(
        !directory
            .path()
            .parent()
            .unwrap()
            .join("escape.txt")
            .exists()
    );

    write
        .execute(&context, json!({"path":"inside.txt","content":"ok"}))
        .await
        .unwrap();
    let read = registry.get("read_file").unwrap();
    let result = read
        .execute(&context, json!({"path":"inside.txt"}))
        .await
        .unwrap();
    assert_eq!(result.output["content"], "ok");
}

/// A link inside the workspace that points outside it must fail closed at
/// I/O time — the capability dir resolves each component against the root
/// handle, so the check-to-use race the path validation used to have is gone.
#[tokio::test]
async fn file_tools_reject_links_pointing_outside_the_workspace() {
    let directory = tempdir().unwrap();
    let outside = tempdir().unwrap();
    std::fs::write(outside.path().join("secret.txt"), "top-secret").unwrap();
    let link = directory.path().join("link");
    #[cfg(windows)]
    {
        std::process::Command::new("cmd.exe")
            .args([
                "/D",
                "/C",
                "mklink",
                "/J",
                &link.to_string_lossy(),
                &outside.path().to_string_lossy(),
            ])
            .status()
            .expect("junction creation requires no special privilege");
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(outside.path(), &link).unwrap();
    }
    #[cfg(not(any(windows, unix)))]
    return;

    let registry = builtin_registry();
    let context = context(directory.path());
    let read = registry.get("read_file").unwrap();
    let outcome = read
        .execute(&context, json!({"path":"link/secret.txt"}))
        .await;
    match outcome {
        Err(_) => {}
        Ok(result) => {
            assert!(
                result.is_error,
                "escaping link must fail closed: {:?}",
                result.output
            );
        }
    }
    let outcome = read
        .execute(&context, json!({"path":"link/secret.txt"}))
        .await;
    if let Ok(result) = outcome {
        assert!(!result.output.to_string().contains("top-secret"));
    }

    let write = registry.get("write_file").unwrap();
    let escaped = write.execute(&context, json!({"path":"link/planted.txt","content":"no"}));
    if let Ok(result) = escaped.await {
        assert!(result.is_error, "escaping write must fail closed");
    }
    assert!(
        !outside.path().join("planted.txt").exists(),
        "write through an escaping link must not touch the outside filesystem"
    );

    // Links that stay inside the workspace keep working. Windows junctions
    // carry an NT-namespace target that fails closed through the capability
    // dir, so the positive case is asserted on Unix only — failing closed is
    // still the safe outcome on Windows. The inner link uses a *relative*
    // target: capability resolution (openat2 RESOLVE_BENEATH) rejects
    // absolute symlinks even when they point back inside the workspace.
    #[cfg(unix)]
    {
        let inner = directory.path().join("real");
        std::fs::create_dir(&inner).unwrap();
        std::fs::write(inner.join("ok.txt"), "inner").unwrap();
        let inner_link = directory.path().join("inner-link");
        std::os::unix::fs::symlink("real", &inner_link).unwrap();
        let ok = read
            .execute(&context, json!({"path":"inner-link/ok.txt"}))
            .await;
        assert_eq!(ok.unwrap().output["content"], "inner");
    }
}

#[tokio::test]
async fn sandbox_process_has_no_host_secret_without_explicit_allow() {
    let directory = tempdir().unwrap();
    let registry = builtin_registry();
    let mut context = context(directory.path());
    context.allow_trusted_host_processes = true;
    context.environment = HashMap::from([
        ("OPENAI_API_KEY".to_owned(), "sk-never-leak".to_owned()),
        ("SAFE_VALUE".to_owned(), "visible".to_owned()),
    ]);
    let shell = registry.get("shell").unwrap();
    #[cfg(windows)]
    let arguments = json!({
        "program": "cmd.exe",
        "args": ["/D", "/C", "echo %OPENAI_API_KEY%:%SAFE_VALUE%"]
    });
    #[cfg(not(windows))]
    let arguments = json!({
        "program": "/bin/sh",
        "args": ["-c", "printf '%s:%s' \"$OPENAI_API_KEY\" \"$SAFE_VALUE\""]
    });
    let result = shell.execute(&context, arguments).await.unwrap();
    let encoded = result.output.to_string();
    assert!(!encoded.contains("sk-never-leak"));
    assert!(encoded.contains("visible"));

    context.allowed_secret_environment = BTreeSet::from(["OPENAI_API_KEY".to_owned()]);
    let result = shell
        .execute(
            &context,
            if cfg!(windows) {
                json!({"program":"cmd.exe","args":["/D","/C","echo %OPENAI_API_KEY%"]})
            } else {
                json!({"program":"/bin/sh","args":["-c","printf '%s' \"$OPENAI_API_KEY\""]})
            },
        )
        .await
        .unwrap();
    // Output filtering still masks explicitly passed credentials before the model sees them.
    assert!(!result.output.to_string().contains("sk-never-leak"));
}

#[tokio::test]
async fn sandbox_process_cancellation_kills_and_reaps_before_returning() {
    let directory = tempdir().unwrap();
    let registry = builtin_registry();
    let mut context = context(directory.path());
    context.allow_trusted_host_processes = true;
    let (sender, cancel) = CancelSignal::channel();
    context.cancel = Some(cancel);
    let shell = registry.get("shell").unwrap();
    #[cfg(windows)]
    let arguments = json!({"program":"cmd.exe","args":["/D","/C","ping -n 20 127.0.0.1 >nul"]});
    #[cfg(not(windows))]
    let arguments = json!({"program":"/bin/sh","args":["-c","sleep 20"]});
    let task = tokio::spawn(async move { shell.execute(&context, arguments).await });
    sender.send(Some("test".to_owned())).unwrap();
    let result = tokio::time::timeout(std::time::Duration::from_secs(2), task)
        .await
        .expect("cancelled process must be reaped promptly")
        .unwrap();
    assert!(matches!(result, Err(ToolError::Cancelled)));
}

#[tokio::test]
async fn host_process_execution_fails_closed_without_explicit_trusted_host_opt_in() {
    let directory = tempdir().unwrap();
    let registry = builtin_registry();
    let context = context(directory.path());
    let shell = registry.get("shell").unwrap();
    let result = shell
        .execute(
            &context,
            json!({"program":"definitely-not-started","args":[]}),
        )
        .await;
    assert!(matches!(result, Err(ToolError::Security(_))));
}

/// Killing the bounded child must take the whole containment unit with it —
/// a grandchild that would outlive the parent (a Windows Job Object or a Unix
/// process group) never runs its payload once the tool call is torn down.
#[cfg(any(windows, unix))]
#[tokio::test]
async fn containment_unit_kills_descendants_with_the_child() {
    let directory = tempdir().unwrap();
    let registry = builtin_registry();
    let mut context = context(directory.path());
    context.allow_trusted_host_processes = true;
    context.timeout = Duration::from_millis(500);
    let shell = registry.get("shell").unwrap();
    // The parent spawns a descendant that writes a marker ~4s in, then blocks.
    // The timeout fires at 500ms and must kill both, so the marker never lands.
    #[cfg(windows)]
    let arguments = json!({
        "program": "cmd.exe",
        "args": ["/D", "/C", "start /b cmd /c \"ping -n 5 127.0.0.1 >nul && echo GHOST> ghost.txt\" & ping -n 60 127.0.0.1 >nul"],
    });
    #[cfg(unix)]
    let arguments = json!({
        "program": "/bin/sh",
        "args": ["-c", "(sleep 4 && echo GHOST > ghost.txt) & sleep 60"],
    });
    let result = shell.execute(&context, arguments).await;
    assert!(matches!(result, Err(ToolError::Timeout)));
    tokio::time::sleep(Duration::from_secs(6)).await;
    assert!(
        !directory.path().join("ghost.txt").exists(),
        "a descendant that outlives the child must be killed with it"
    );
}

#[test]
fn capability_fixture_matches_python_precedence_semantics() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/security-parity.json")).unwrap();
    for case in fixture.as_array().unwrap() {
        let wildcard = decision(case["wildcard"].as_str().unwrap());
        let mut policy = CapabilityPolicy::new(Some(wildcard));
        for (name, value) in case["capabilities"].as_object().unwrap() {
            policy.set(capability(name), decision(value.as_str().unwrap()));
        }
        let required = case["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| capability(value.as_str().unwrap()));
        let actual = policy
            .evaluate(required, decision(case["toolDecision"].as_str().unwrap()))
            .effective;
        assert_eq!(actual, decision(case["expected"].as_str().unwrap()));
    }
}

fn decision(value: &str) -> Decision {
    match value {
        "allow" => Decision::Allow,
        "ask" => Decision::Ask,
        "deny" => Decision::Deny,
        other => panic!("unknown decision {other}"),
    }
}

fn capability(value: &str) -> Capability {
    match value {
        "read" => Capability::Read,
        "write" => Capability::Write,
        "execute" => Capability::Execute,
        "network" => Capability::Network,
        "git" => Capability::Git,
        "send_external" => Capability::SendExternal,
        other => panic!("unknown capability {other}"),
    }
}
