use cool_agent::{ToolContext, builtin_registry};
use cool_security::{CapabilityPolicy, Decision, Workspace};
use serde_json::json;
use tempfile::tempdir;

fn context(root: &std::path::Path) -> ToolContext {
    ToolContext::new(
        Workspace::new(root).unwrap(),
        CapabilityPolicy::new(Some(Decision::Allow)),
    )
}

fn write(root: &std::path::Path, path: &str, bytes: &[u8]) {
    let target = root.join(path);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(target, bytes).unwrap();
}

#[tokio::test]
async fn search_files_finds_regex_matches_with_context() {
    let directory = tempdir().unwrap();
    write(
        directory.path(),
        "src/main.rs",
        b"fn alpha() {}\nlet target = 1;\nfn omega() {}\n",
    );
    write(directory.path(), "src/lib.rs", b"let target = 2;\n");
    let registry = builtin_registry();
    let context = context(directory.path());
    let search = registry.get("search_files").unwrap();
    let result = search
        .execute(&context, json!({"pattern": "targ\\w+"}))
        .await
        .unwrap();
    assert!(!result.is_error);
    assert!(!result.truncated);
    let matches = result.output["matches"].as_array().unwrap();
    assert_eq!(matches.len(), 2);
    assert_eq!(result.output["totalMatches"], 2);
    assert_eq!(matches[0]["path"], "src/lib.rs");
    assert_eq!(matches[0]["line"], 1);
    assert_eq!(matches[0]["column"], 5);
    assert_eq!(matches[1]["path"], "src/main.rs");
    assert_eq!(matches[1]["line"], 2);

    let result = search
        .execute(
            &context,
            json!({"pattern": "target", "path": "src/main.rs", "context": 1}),
        )
        .await
        .unwrap();
    let matches = result.output["matches"].as_array().unwrap();
    assert_eq!(matches.len(), 1);
    let context_lines = matches[0]["context"].as_array().unwrap();
    assert_eq!(context_lines.len(), 2);
    assert_eq!(context_lines[0]["text"], "fn alpha() {}");
    assert_eq!(context_lines[1]["text"], "fn omega() {}");
}

#[tokio::test]
async fn search_files_glob_narrows_results() {
    let directory = tempdir().unwrap();
    write(directory.path(), "a/match.rs", b"needle\n");
    write(directory.path(), "b/match.py", b"needle\n");
    let registry = builtin_registry();
    let context = context(directory.path());
    let search = registry.get("search_files").unwrap();
    let result = search
        .execute(&context, json!({"pattern": "needle", "glob": "*.rs"}))
        .await
        .unwrap();
    let matches = result.output["matches"].as_array().unwrap();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0]["path"], "a/match.rs");
}

#[tokio::test]
async fn search_files_respects_gitignore_and_skips_non_utf8_and_large() {
    let directory = tempdir().unwrap();
    // The ignore walker only honors .gitignore inside a git checkout.
    std::fs::create_dir(directory.path().join(".git")).unwrap();
    write(directory.path(), ".gitignore", b"ignored/\n");
    write(directory.path(), "ignored/hidden.txt", b"needle\n");
    write(directory.path(), "kept/visible.txt", b"needle\n");
    write(directory.path(), "binary.dat", b"needle\xff\xfe\x00");
    write(directory.path(), "huge.txt", &vec![b'x'; 5 * 1024 * 1024]);
    let registry = builtin_registry();
    let context = context(directory.path());
    let search = registry.get("search_files").unwrap();
    let result = search
        .execute(&context, json!({"pattern": "needle"}))
        .await
        .unwrap();
    let matches = result.output["matches"].as_array().unwrap();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0]["path"], "kept/visible.txt");
    assert_eq!(result.output["skippedLargeFiles"], 1);
}

#[tokio::test]
async fn search_files_rejects_escapes_and_bounds_results() {
    let directory = tempdir().unwrap();
    for index in 0..5 {
        write(directory.path(), &format!("f{index}.txt"), b"needle\n");
    }
    let registry = builtin_registry();
    let context = context(directory.path());
    let search = registry.get("search_files").unwrap();
    let denied = search
        .execute(&context, json!({"pattern": "needle", "path": "../"}))
        .await;
    assert!(denied.is_err() && denied.unwrap_err().to_string().contains("security"));

    let result = search
        .execute(&context, json!({"pattern": "needle", "maxResults": 2}))
        .await
        .unwrap();
    assert_eq!(result.output["matches"].as_array().unwrap().len(), 2);
    assert_eq!(result.output["totalMatches"], 5);
    assert_eq!(result.output["truncated"], true);
    assert!(result.truncated);

    let invalid = search
        .execute(&context, json!({"pattern": "([", "maxResults": 2}))
        .await;
    assert!(invalid.is_err());
}

#[tokio::test]
async fn find_files_matches_glob_and_bounds_results() {
    let directory = tempdir().unwrap();
    write(directory.path(), "src/a.rs", b"");
    write(directory.path(), "src/deep/b.rs", b"");
    write(directory.path(), "docs/c.md", b"");
    write(directory.path(), "ignored.txt", b"");
    std::fs::create_dir(directory.path().join(".git")).unwrap();
    write(directory.path(), ".gitignore", b"ignored.txt\n");
    let registry = builtin_registry();
    let context = context(directory.path());
    let find = registry.get("find_files").unwrap();
    let result = find
        .execute(&context, json!({"glob": "**/*.rs"}))
        .await
        .unwrap();
    assert!(!result.is_error);
    let paths: Vec<&str> = result.output["paths"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect();
    assert_eq!(paths, ["src/a.rs", "src/deep/b.rs"]);

    let denied = find
        .execute(&context, json!({"glob": "**/*.rs", "path": "../"}))
        .await;
    assert!(denied.is_err() && denied.unwrap_err().to_string().contains("security"));

    let result = find
        .execute(&context, json!({"glob": "**/*", "maxResults": 1}))
        .await
        .unwrap();
    assert_eq!(result.output["paths"].as_array().unwrap().len(), 1);
    assert_eq!(result.output["truncated"], true);
    assert!(result.truncated);
}

#[tokio::test]
async fn read_file_pages_with_offset_bytes() {
    let directory = tempdir().unwrap();
    write(directory.path(), "data.txt", b"0123456789");
    let registry = builtin_registry();
    let context = context(directory.path());
    let read = registry.get("read_file").unwrap();

    let result = read
        .execute(
            &context,
            json!({"path": "data.txt", "offset_bytes": 4, "maxBytes": 3}),
        )
        .await
        .unwrap();
    assert_eq!(result.output["content"], "456");
    assert_eq!(result.output["totalBytes"], 10);
    assert_eq!(result.output["truncated"], true);
    assert!(result.truncated);
    assert_eq!(result.output["hint"], "use offset_bytes/max_bytes to page");

    let tail = read
        .execute(
            &context,
            json!({"path": "data.txt", "offset_bytes": 7, "maxBytes": 10}),
        )
        .await
        .unwrap();
    assert_eq!(tail.output["content"], "789");
    assert_eq!(tail.output["truncated"], false);
    assert!(tail.output.get("hint").is_none());
}

/// Over-cap process output must land in `.cool/spill/` and return a head/tail
/// view instead of dropping everything.
#[tokio::test]
async fn oversized_process_output_spills_to_workspace_file() {
    let directory = tempdir().unwrap();
    let registry = builtin_registry();
    let mut context = context(directory.path());
    context.allow_trusted_host_processes = true;
    context.max_output_bytes = 2048;
    let shell = registry.get("shell").unwrap();
    #[cfg(windows)]
    let arguments = json!({
        "program": "cmd.exe",
        "args": ["/D", "/C", "for /l %i in (1,1,4000) do @echo %i"]
    });
    #[cfg(not(windows))]
    let arguments = json!({
        "program": "/bin/sh",
        "args": ["-c", "i=0; while [ $i -lt 4000 ]; do echo $i; i=$((i+1)); done"]
    });
    let result = shell.execute(&context, arguments).await.unwrap();
    assert!(!result.is_error);
    assert!(result.truncated);
    assert_eq!(result.output["truncated"], true);
    let stdout = result.output["stdout"].as_str().unwrap();
    assert!(stdout.contains("[truncated"));
    assert!(stdout.contains(".cool/spill/"));
    let spill_path = result.output["stdoutSpillPath"].as_str().unwrap();
    let spill = std::fs::read_to_string(directory.path().join(spill_path)).unwrap();
    assert!(spill.len() > stdout.len());
    assert!(spill.contains("3999"));
}

/// The new tools are read-scoped and allowed by default.
#[tokio::test]
async fn search_and_find_require_read_capability() {
    let registry = builtin_registry();
    for name in ["search_files", "find_files"] {
        let tool = registry.get(name).unwrap();
        assert!(tool.capabilities.contains(&cool_security::Capability::Read));
        assert_eq!(tool.default_decision, Decision::Allow);
    }
}
