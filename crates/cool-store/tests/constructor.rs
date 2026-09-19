//! Macro tool (Agent Constructor) store parity tests.

mod common;

use common::{DEFAULT_SEED, create_python_database, temp_database};
use cool_store::domains::constructor::{MacroToolPatch, NewMacroTool};
use cool_store::{LegacyStore, StoreError};
use serde_json::json;
use tempfile::TempDir;

fn adopted_store() -> (TempDir, LegacyStore) {
    let directory = TempDir::new().expect("tempdir");
    let path = temp_database(directory.path(), "harness.db");
    create_python_database(&path, DEFAULT_SEED);
    let store = LegacyStore::open(
        &path,
        &cool_store::StoreOptions {
            initialize_if_missing: false,
            ..cool_store::StoreOptions::default()
        },
    )
    .expect("adopt");
    (directory, store)
}

fn macro_payload(name: &str) -> NewMacroTool {
    NewMacroTool {
        name: name.to_string(),
        description: format!("{name} description"),
        input_schema: json!({"type": "object", "properties": {"path": {"type": "string"}}}),
        steps: json!([{"id": "read", "tool_name": "read_file", "arguments": {"path": "${input.path}"}}]),
        ..NewMacroTool::default()
    }
}

#[test]
fn macro_tool_crud_ordering_and_conflict() {
    let (_directory, store) = adopted_store();
    let zeta = store
        .create_macro_tool("local-user", &macro_payload("macro_zeta"))
        .expect("create zeta");
    let alpha = store
        .create_macro_tool("local-user", &macro_payload("macro_alpha"))
        .expect("create alpha");
    assert!(alpha.is_active);
    assert_eq!(alpha.user_id, 1);

    let listed = store.list_macro_tools("local-user", true).expect("list");
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].id, alpha.id);
    assert_eq!(listed[1].id, zeta.id);

    let duplicate = store
        .create_macro_tool("local-user", &macro_payload("macro_alpha"))
        .expect_err("duplicate name");
    assert!(matches!(duplicate, StoreError::Conflict(_)));

    let updated = store
        .update_macro_tool(
            "local-user",
            zeta.id,
            &MacroToolPatch {
                description: Some("patched".to_string()),
                is_active: Some(false),
                steps: Some(json!([{"id": "list", "tool_name": "list_files", "arguments": {}}])),
                ..MacroToolPatch::default()
            },
        )
        .expect("update");
    assert_eq!(updated.description, "patched");
    assert!(!updated.is_active);
    assert_eq!(
        updated.steps,
        json!([{"id": "list", "tool_name": "list_files", "arguments": {}}])
    );

    let active = store.list_macro_tools("local-user", false).expect("active");
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].id, alpha.id);

    store
        .delete_macro_tool("local-user", zeta.id)
        .expect("delete");
    let gone = store
        .get_macro_tool("local-user", zeta.id)
        .expect_err("gone");
    assert!(matches!(gone, StoreError::NotFound("macro tool")));
}

#[test]
fn macro_tools_are_actor_scoped() {
    let (_directory, store) = adopted_store();
    let owned = store
        .create_macro_tool("local-user", &macro_payload("macro_shared"))
        .expect("create");
    store.ensure_actor("other-actor").expect("register actor");

    assert!(
        store
            .list_macro_tools("other-actor", true)
            .expect("list other")
            .is_empty()
    );
    let error = store
        .get_macro_tool("other-actor", owned.id)
        .expect_err("cross actor");
    assert!(matches!(error, StoreError::NotFound("macro tool")));
    let update = store
        .update_macro_tool(
            "other-actor",
            owned.id,
            &MacroToolPatch {
                is_active: Some(false),
                ..MacroToolPatch::default()
            },
        )
        .expect_err("cross actor update");
    assert!(matches!(update, StoreError::NotFound("macro tool")));
    let delete = store
        .delete_macro_tool("other-actor", owned.id)
        .expect_err("cross actor delete");
    assert!(matches!(delete, StoreError::NotFound("macro tool")));
}
