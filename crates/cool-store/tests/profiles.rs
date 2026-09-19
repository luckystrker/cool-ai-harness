//! Agent profile store parity tests (Фаза 3a §2).

mod common;

use common::{DEFAULT_SEED, create_python_database, temp_database};
use cool_store::domains::profiles::{AgentProfilePatch, NewAgentProfile};
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

#[test]
fn profile_crud_defaults_ordering_and_conflict() {
    let (_directory, store) = adopted_store();

    let custom = store
        .create_profile(&NewAgentProfile {
            name: "Zeta".to_string(),
            slug: "zeta".to_string(),
            description: Some("custom".to_string()),
            tool_names: Some(json!(["read_file"])),
            ..NewAgentProfile::default()
        })
        .expect("create custom");
    assert!(custom.is_active);
    assert!(!custom.is_builtin);
    assert!(!custom.is_shared);

    let builtin = store
        .create_profile(&NewAgentProfile {
            name: "Alpha".to_string(),
            slug: "alpha".to_string(),
            is_builtin: true,
            ..NewAgentProfile::default()
        })
        .expect("create builtin");

    let listed = store.list_profiles(true).expect("list");
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].id, builtin.id, "built-ins sort first");
    assert_eq!(listed[1].id, custom.id);

    let duplicate = store
        .create_profile(&NewAgentProfile {
            name: "Dup".to_string(),
            slug: "zeta".to_string(),
            ..NewAgentProfile::default()
        })
        .expect_err("duplicate slug");
    assert!(matches!(duplicate, StoreError::Conflict(_)));

    let found = store
        .find_profile_by_slug("zeta")
        .expect("find")
        .expect("present");
    assert_eq!(found.id, custom.id);
    assert!(
        store
            .find_profile_by_slug("missing")
            .expect("find")
            .is_none()
    );

    let updated = store
        .update_profile(
            custom.id,
            &AgentProfilePatch {
                name: Some("Zeta Prime".to_string()),
                model: Some("gpt-x".to_string()),
                skill_names: Some(json!(["summarize"])),
                is_shared: Some(true),
                ..AgentProfilePatch::default()
            },
        )
        .expect("update");
    assert_eq!(updated.name, "Zeta Prime");
    assert_eq!(updated.model.as_deref(), Some("gpt-x"));
    assert_eq!(updated.skill_names, Some(json!(["summarize"])));
    assert!(updated.is_shared);

    store
        .update_profile(
            custom.id,
            &AgentProfilePatch {
                slug: Some("zeta".to_string()),
                ..AgentProfilePatch::default()
            },
        )
        .expect("re-setting the same slug is allowed");

    store
        .update_profile(
            custom.id,
            &AgentProfilePatch {
                model: Some(String::new()),
                settings: Some(json!({})),
                ..AgentProfilePatch::default()
            },
        )
        .expect("clear fields");
    let cleared = store.get_profile(custom.id).expect("get");
    assert_eq!(cleared.model, None);
    assert_eq!(cleared.settings, None);

    store
        .update_profile(
            custom.id,
            &AgentProfilePatch {
                is_active: Some(false),
                ..AgentProfilePatch::default()
            },
        )
        .expect("deactivate");
    let active = store.list_profiles(false).expect("active");
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].id, builtin.id);
    assert_eq!(store.list_profiles(true).expect("all").len(), 2);
}

#[test]
fn profile_builtin_delete_policy_and_clone() {
    let (_directory, store) = adopted_store();
    let source = store
        .create_profile(&NewAgentProfile {
            name: "Coder".to_string(),
            slug: "coder".to_string(),
            system_prompt: Some("You code.".to_string()),
            tool_names: Some(json!(["read_file", "write_file"])),
            avatar_color: Some("#3B82F6".to_string()),
            ..NewAgentProfile::default()
        })
        .expect("source");
    let builtin = store
        .create_profile(&NewAgentProfile {
            name: "Builtin".to_string(),
            slug: "builtin".to_string(),
            is_builtin: true,
            ..NewAgentProfile::default()
        })
        .expect("builtin");

    let error = store
        .delete_profile(builtin.id)
        .expect_err("builtin delete");
    assert!(matches!(error, StoreError::InvalidInput(_)));

    let clone = store
        .clone_profile(source.id, "Coder Copy", "coder-copy")
        .expect("clone");
    assert_eq!(clone.name, "Coder Copy");
    assert_eq!(clone.slug, "coder-copy");
    assert_eq!(clone.system_prompt.as_deref(), Some("You code."));
    assert_eq!(clone.tool_names, Some(json!(["read_file", "write_file"])));
    assert!(!clone.is_builtin);
    assert!(clone.is_active);
    assert!(!clone.is_shared);

    let duplicate = store
        .clone_profile(source.id, "Again", "coder-copy")
        .expect_err("clone conflict");
    assert!(matches!(duplicate, StoreError::Conflict(_)));

    store.delete_profile(source.id).expect("delete source");
    let gone = store.get_profile(source.id).expect_err("gone");
    assert!(matches!(gone, StoreError::NotFound("profile")));
}
