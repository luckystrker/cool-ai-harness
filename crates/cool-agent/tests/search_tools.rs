use cool_agent::{HostLauncher, ToolContext, builtin_registry};
use cool_security::{Capability, CapabilityPolicy, Decision, Workspace};
use serde_json::{Value, json};
use std::sync::Arc;
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

/// Secrets on lines adjacent to a match must be masked like everything else,
/// and a huge `context` request is bounded instead of exploding output size.
#[tokio::test]
async fn search_files_context_lines_are_masked_and_bounded() {
    let directory = tempdir().unwrap();
    let mut content = String::new();
    for index in 0..30 {
        content.push_str(&format!("filler {index}\n"));
    }
    content.push_str("password=hunter2hunter2\nneedle\n");
    write(directory.path(), "app.rs", content.as_bytes());
    let registry = builtin_registry();
    let context = context(directory.path());
    let search = registry.get("search_files").unwrap();
    let result = search
        .execute(&context, json!({"pattern": "needle", "context": 999}))
        .await
        .unwrap();
    let matches = result.output["matches"].as_array().unwrap();
    let context_lines = matches[0]["context"].as_array().unwrap();
    // context is capped at 10 lines before/after.
    assert_eq!(context_lines.len(), 10);
    let last = context_lines.last().unwrap();
    assert_eq!(last["line"], 31);
    assert!(last["text"].as_str().unwrap().contains("[REDACTED]"));
    assert!(!result.output.to_string().contains("hunter2hunter2"));
}

/// Every hit on a line gets its own match entry with its own column.
#[tokio::test]
async fn search_files_reports_every_match_on_a_line() {
    let directory = tempdir().unwrap();
    write(
        directory.path(),
        "hits.txt",
        b"aa marker bb marker\nplain line\n",
    );
    let registry = builtin_registry();
    let context = context(directory.path());
    let search = registry.get("search_files").unwrap();
    let result = search
        .execute(&context, json!({"pattern": "marker"}))
        .await
        .unwrap();
    assert_eq!(result.output["totalMatches"], 2);
    let matches = result.output["matches"].as_array().unwrap();
    assert_eq!(matches.len(), 2);
    assert_eq!(matches[0]["column"], 4);
    assert_eq!(matches[1]["column"], 14);
}

/// A search root that is a link pointing outside the workspace must fail
/// closed rather than enumerate external filenames.
#[tokio::test]
async fn symlinked_search_root_fails_closed() {
    let directory = tempdir().unwrap();
    let outside = tempdir().unwrap();
    std::fs::write(outside.path().join("external.txt"), "needle").unwrap();
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
    for tool in ["search_files", "find_files"] {
        let handler = registry.get(tool).unwrap();
        let arguments = if tool == "search_files" {
            json!({"pattern": "needle", "path": "link"})
        } else {
            json!({"glob": "**/*.txt", "path": "link"})
        };
        let outcome = handler.execute(&context, arguments).await;
        match outcome {
            Err(_) => {}
            Ok(result) => assert!(
                result.is_error || !result.output.to_string().contains("external.txt"),
                "{tool} must not enumerate a symlinked root outside the workspace: {:?}",
                result.output
            ),
        }
    }
}

/// When the serialized result exceeds the 10MB spill cap, the spilled file is
/// trimmed to complete, parseable JSON rather than a cut-off blob.
#[tokio::test]
async fn oversized_search_results_spill_parseable_json() {
    let directory = tempdir().unwrap();
    let mut content = String::new();
    for _ in 0..1990 {
        content.push_str(&"m".repeat(2000));
        content.push('\n');
    }
    write(directory.path(), "big.txt", content.as_bytes());
    let registry = builtin_registry();
    let context = context(directory.path());
    let search = registry.get("search_files").unwrap();
    let result = search
        .execute(
            &context,
            json!({"pattern": "m+", "context": 10, "maxResults": 2000}),
        )
        .await
        .unwrap();
    assert!(result.truncated);
    assert_eq!(result.output["totalMatches"], 1990);
    let spill_path = result.output["spillPath"].as_str().unwrap();
    let spill = std::fs::read(directory.path().join(spill_path)).unwrap();
    assert!(spill.len() <= 10 * 1024 * 1024, "spill exceeded cap");
    let parsed: Value = serde_json::from_slice(&spill).expect("spilled result must be valid JSON");
    assert!(parsed["matches"].is_array());
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
    context.launcher = Arc::new(HostLauncher);
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

/// stdout and stderr share the output cap: when stdout consumes the budget,
/// stderr spills even though it is individually under the limit.
#[tokio::test]
async fn combined_process_output_respects_the_shared_cap() {
    let directory = tempdir().unwrap();
    write(directory.path(), "out.txt", vec![b'o'; 4096].as_slice());
    write(directory.path(), "err.txt", vec![b'e'; 4096].as_slice());
    let registry = builtin_registry();
    let mut context = context(directory.path());
    context.launcher = Arc::new(HostLauncher);
    context.max_output_bytes = 4096;
    context.timeout = std::time::Duration::from_secs(120);
    let shell = registry.get("shell").unwrap();
    #[cfg(windows)]
    let arguments = json!({
        "program": "cmd.exe",
        "args": ["/D", "/C", "type out.txt & type err.txt 1>&2"]
    });
    #[cfg(not(windows))]
    let arguments = json!({
        "program": "/bin/sh",
        "args": ["-c", "cat out.txt; cat err.txt 1>&2"]
    });
    let result = shell.execute(&context, arguments).await.unwrap();
    assert!(result.truncated);
    let stdout = result.output["stdout"].as_str().unwrap();
    let stderr = result.output["stderr"].as_str().unwrap();
    assert!(
        stdout.len() + stderr.len() <= 4096,
        "combined output exceeds cap: {} + {}",
        stdout.len(),
        stderr.len()
    );
    // stderr was pushed over the remaining budget and spilled in full.
    let spill_path = result.output["stderrSpillPath"].as_str().unwrap();
    let spill = std::fs::read(directory.path().join(spill_path)).unwrap();
    assert_eq!(spill.len(), 4096);
}

/// Reused call ids must not let one spill overwrite another.
#[tokio::test]
async fn spill_filenames_are_unique_per_call() {
    let directory = tempdir().unwrap();
    write(directory.path(), "out.txt", vec![b'o'; 4096].as_slice());
    let registry = builtin_registry();
    let mut context = context(directory.path());
    context.launcher = Arc::new(HostLauncher);
    context.max_output_bytes = 512;
    context.timeout = std::time::Duration::from_secs(120);
    context.call_id = Some("reused-id".to_owned());
    let shell = registry.get("shell").unwrap();
    let mut paths = Vec::new();
    for _ in 0..2 {
        #[cfg(windows)]
        let arguments = json!({"program": "cmd.exe", "args": ["/D", "/C", "type out.txt"]});
        #[cfg(not(windows))]
        let arguments = json!({"program": "/bin/sh", "args": ["-c", "cat out.txt"]});
        let result = shell.execute(&context, arguments).await.unwrap();
        paths.push(
            result.output["stdoutSpillPath"]
                .as_str()
                .unwrap()
                .to_owned(),
        );
    }
    assert_ne!(paths[0], paths[1], "reused call_id must not collide");
    for path in &paths {
        assert!(directory.path().join(path).exists(), "missing {path}");
    }
}

/// A read-only tool must not write spill files when the run's policy denies
/// Write — the truncated result stays in-band with a marker instead.
#[tokio::test]
async fn read_only_tool_skips_spill_when_write_is_denied() {
    let directory = tempdir().unwrap();
    let mut content = String::new();
    for index in 0..200 {
        content.push_str(&format!("needle line {index}\n"));
    }
    write(directory.path(), "dense.txt", content.as_bytes());
    let registry = builtin_registry();
    let mut context = context(directory.path());
    context.max_output_bytes = 512;
    context.policy.set(Capability::Write, Decision::Deny);
    let search = registry.get("search_files").unwrap();
    let result = search
        .execute(&context, json!({"pattern": "needle"}))
        .await
        .unwrap();
    assert!(result.truncated);
    assert_eq!(
        result.output["spillSkipped"],
        "Write capability denied by policy"
    );
    assert!(result.output.get("spillPath").is_none());
    assert!(
        !directory.path().join(".cool").exists(),
        "denied spill must not create .cool/"
    );
}

/// A stream larger than the 10MB spill cap still keeps its true ending: the
/// spill file and the returned tail both carry the final bytes.
#[tokio::test]
async fn overspill_process_output_keeps_the_true_tail() {
    let directory = tempdir().unwrap();
    let mut big = vec![b'x'; 12 * 1024 * 1024];
    big.extend_from_slice(b"TAIL_END_MARKER");
    write(directory.path(), "big.txt", &big);
    let registry = builtin_registry();
    let mut context = context(directory.path());
    context.launcher = Arc::new(HostLauncher);
    let shell = registry.get("shell").unwrap();
    #[cfg(windows)]
    let arguments = json!({
        "program": "cmd.exe",
        "args": ["/D", "/C", "type big.txt"]
    });
    #[cfg(not(windows))]
    let arguments = json!({
        "program": "/bin/sh",
        "args": ["-c", "cat big.txt"]
    });
    let result = shell.execute(&context, arguments).await.unwrap();
    assert!(result.truncated);
    let stdout = result.output["stdout"].as_str().unwrap();
    assert!(stdout.contains("TAIL_END_MARKER"), "true tail lost");
    let spill_path = result.output["stdoutSpillPath"].as_str().unwrap();
    let spill = std::fs::read(directory.path().join(spill_path)).unwrap();
    assert!(spill.len() <= 10 * 1024 * 1024, "spill exceeded cap");
    let spill = String::from_utf8_lossy(&spill);
    assert!(spill.contains("TAIL_END_MARKER"), "spill lost the tail");
    assert!(spill.contains("bytes omitted mid-stream"));
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
