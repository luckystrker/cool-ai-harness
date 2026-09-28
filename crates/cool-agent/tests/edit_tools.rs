use cool_agent::{ToolContext, builtin_registry};
use cool_security::{CapabilityPolicy, Decision, Workspace};
use serde_json::{Value, json};
use tempfile::tempdir;

fn context(root: &std::path::Path) -> ToolContext {
    ToolContext::new(
        Workspace::new(root).unwrap(),
        CapabilityPolicy::new(Some(Decision::Allow)),
    )
}

fn write(root: &std::path::Path, path: &str, text: &str) {
    let target = root.join(path);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(target, text).unwrap();
}

fn edit_args(path: &str, edits: Value) -> Value {
    json!({"path": path, "edits": edits})
}

#[tokio::test]
async fn edit_file_replaces_a_unique_anchor() {
    let directory = tempdir().unwrap();
    write(
        directory.path(),
        "main.rs",
        "fn main() {\n    old_call();\n}\n",
    );
    let registry = builtin_registry();
    let edit = registry.get("edit_file").unwrap();
    let result = edit
        .execute(
            &context(directory.path()),
            edit_args(
                "main.rs",
                json!([{"old": "old_call()", "new": "new_call()"}]),
            ),
        )
        .await
        .unwrap();
    assert!(!result.is_error);
    assert_eq!(result.output["editsApplied"], 1);
    assert_eq!(result.output["bytesBefore"], 30);
    let diff = result.output["diff"].as_str().unwrap();
    assert!(diff.contains("@@ -1,4 +1,4 @@"));
    assert!(diff.contains("-    old_call();"));
    assert!(diff.contains("+    new_call();"));
    assert!(diff.contains("--- a/main.rs"));
    assert_eq!(
        std::fs::read_to_string(directory.path().join("main.rs")).unwrap(),
        "fn main() {\n    new_call();\n}\n"
    );
}

#[tokio::test]
async fn edit_file_rejects_a_non_unique_anchor_with_count() {
    let directory = tempdir().unwrap();
    write(
        directory.path(),
        "app.txt",
        "marker one\nmarker two\nmarker three\n",
    );
    let registry = builtin_registry();
    let edit = registry.get("edit_file").unwrap();
    let result = edit
        .execute(
            &context(directory.path()),
            edit_args("app.txt", json!([{"old": "marker", "new": "hit"}])),
        )
        .await
        .unwrap();
    assert!(result.is_error);
    assert_eq!(result.error_code.as_deref(), Some("edit_not_unique"));
    assert_eq!(result.output["errorCode"], "edit_not_unique");
    assert_eq!(result.output["occurrences"], 3);
    // The file is left untouched when an edit is rejected.
    assert_eq!(
        std::fs::read_to_string(directory.path().join("app.txt")).unwrap(),
        "marker one\nmarker two\nmarker three\n"
    );
}

#[tokio::test]
async fn edit_file_is_atomic_across_the_edit_list() {
    let directory = tempdir().unwrap();
    write(directory.path(), "pair.txt", "alpha\nbeta\ngamma\n");
    let registry = builtin_registry();
    let edit = registry.get("edit_file").unwrap();
    let result = edit
        .execute(
            &context(directory.path()),
            edit_args(
                "pair.txt",
                json!([
                    {"old": "alpha", "new": "ALPHA"},
                    {"old": "missing-anchor", "new": "nope"},
                ]),
            ),
        )
        .await
        .unwrap();
    assert!(result.is_error);
    assert_eq!(result.error_code.as_deref(), Some("edit_not_found"));
    assert_eq!(result.output["editIndex"], 1);
    // The first, valid edit must not leak through: the whole call is atomic.
    assert_eq!(
        std::fs::read_to_string(directory.path().join("pair.txt")).unwrap(),
        "alpha\nbeta\ngamma\n"
    );
}

#[tokio::test]
async fn edit_file_replace_all_swaps_every_occurrence() {
    let directory = tempdir().unwrap();
    write(directory.path(), "all.txt", "x=1\ny=x\nz=x\n");
    let registry = builtin_registry();
    let edit = registry.get("edit_file").unwrap();
    let result = edit
        .execute(
            &context(directory.path()),
            edit_args(
                "all.txt",
                json!([{"old": "x", "new": "W", "replace_all": true}]),
            ),
        )
        .await
        .unwrap();
    assert!(!result.is_error);
    assert_eq!(
        std::fs::read_to_string(directory.path().join("all.txt")).unwrap(),
        "W=1\ny=W\nz=W\n"
    );
}

#[tokio::test]
async fn edit_file_create_if_missing_writes_new_file() {
    let directory = tempdir().unwrap();
    let registry = builtin_registry();
    let edit = registry.get("edit_file").unwrap();
    let mut args = edit_args(
        "nested/new.txt",
        json!([{"old": "", "new": "fresh content\n"}]),
    );
    args["create_if_missing"] = json!(true);
    let result = edit
        .execute(&context(directory.path()), args)
        .await
        .unwrap();
    assert!(!result.is_error);
    assert_eq!(result.output["created"], true);
    assert_eq!(result.output["editsApplied"], 1);
    let diff = result.output["diff"].as_str().unwrap();
    assert!(diff.contains("+fresh content"));
    assert_eq!(
        std::fs::read_to_string(directory.path().join("nested/new.txt")).unwrap(),
        "fresh content\n"
    );

    // Missing file without the flag is a plain not-found error.
    let result = edit
        .execute(
            &context(directory.path()),
            edit_args("other.txt", json!([{"old": "", "new": "x"}])),
        )
        .await
        .unwrap();
    assert!(result.is_error);
    assert_eq!(result.error_code.as_deref(), Some("file_not_found"));

    // A missing file with the flag but a non-empty anchor is rejected, not created.
    let mut args = edit_args("bad.txt", json!([{"old": "seed", "new": "x"}]));
    args["create_if_missing"] = json!(true);
    let result = edit
        .execute(&context(directory.path()), args)
        .await
        .unwrap();
    assert!(result.is_error);
    assert_eq!(result.error_code.as_deref(), Some("edit_invalid_create"));
    assert!(!directory.path().join("bad.txt").exists());
}

#[tokio::test]
async fn edit_file_rejects_empty_anchor_and_paths_outside_workspace() {
    let directory = tempdir().unwrap();
    write(directory.path(), "exists.txt", "body\n");
    let registry = builtin_registry();
    let edit = registry.get("edit_file").unwrap();
    let result = edit
        .execute(
            &context(directory.path()),
            edit_args("exists.txt", json!([{"old": "", "new": "x"}])),
        )
        .await
        .unwrap();
    assert!(result.is_error);
    assert_eq!(result.error_code.as_deref(), Some("edit_empty_anchor"));

    let error = edit
        .execute(
            &context(directory.path()),
            edit_args("../outside.txt", json!([{"old": "a", "new": "b"}])),
        )
        .await;
    assert!(matches!(error, Err(cool_agent::ToolError::Security(_))));
}

#[tokio::test]
async fn edit_file_sequential_edits_and_diff_rendering() {
    let directory = tempdir().unwrap();
    write(directory.path(), "seq.txt", "one\ntwo\nthree\nfour\nfive\n");
    let registry = builtin_registry();
    let edit = registry.get("edit_file").unwrap();
    let result = edit
        .execute(
            &context(directory.path()),
            edit_args(
                "seq.txt",
                json!([
                    {"old": "two", "new": "2"},
                    // The next edit's anchor matches text produced by the previous edit.
                    {"old": "2\nthree", "new": "2\n3"},
                ]),
            ),
        )
        .await
        .unwrap();
    assert!(!result.is_error);
    assert_eq!(result.output["editsApplied"], 2);
    assert_eq!(
        std::fs::read_to_string(directory.path().join("seq.txt")).unwrap(),
        "one\n2\n3\nfour\nfive\n"
    );
    // Both nearby changes merge into one hunk.
    let diff = result.output["diff"].as_str().unwrap();
    assert!(diff.contains("-two"));
    assert!(diff.contains("+2"));
    assert!(diff.contains("-three"));
    assert!(diff.contains("+3"));
    assert_eq!(diff.matches("@@ -").count(), 1);
}

#[tokio::test]
async fn edit_file_writes_atomically_and_leaves_no_temp_files() {
    let directory = tempdir().unwrap();
    write(
        directory.path(),
        "main.rs",
        "fn main() {\n    old_call();\n}\n",
    );
    let registry = builtin_registry();
    let edit = registry.get("edit_file").unwrap();
    let result = edit
        .execute(
            &context(directory.path()),
            edit_args(
                "main.rs",
                json!([{"old": "old_call()", "new": "new_call()"}]),
            ),
        )
        .await
        .unwrap();
    assert!(!result.is_error);
    assert_eq!(
        std::fs::read_to_string(directory.path().join("main.rs")).unwrap(),
        "fn main() {\n    new_call();\n}\n"
    );
    let leftovers: Vec<String> = std::fs::read_dir(directory.path())
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains(".cool-edit-"))
        .collect();
    assert!(leftovers.is_empty(), "atomic write littered: {leftovers:?}");
}
