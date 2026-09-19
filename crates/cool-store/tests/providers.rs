//! Providers store parity tests.

mod common;

use common::{DEFAULT_SEED, create_python_database, temp_database};
use cool_store::domains::providers::{NewProvider, ProviderPatch};
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

fn provider(name: &str, is_active: bool) -> NewProvider {
    NewProvider {
        name: name.to_string(),
        is_active,
        ..NewProvider::default()
    }
}

#[test]
fn provider_crud_filters_inactive_and_patches_fields() {
    let (_directory, store) = adopted_store();
    assert!(
        store
            .list_providers("local-user", true)
            .expect("list")
            .is_empty()
    );

    let active = store
        .create_provider(
            "local-user",
            &NewProvider {
                label: Some("OpenAI".to_string()),
                api_key_encrypted: Some("enc-secret".to_string()),
                default_model: Some("gpt-4o".to_string()),
                ..provider("openai", true)
            },
        )
        .expect("create active");
    let inactive = store
        .create_provider("local-user", &provider("groq", false))
        .expect("create inactive");

    // Debug output must not leak the encrypted API key material.
    let debugged = format!("{active:?}");
    assert!(debugged.contains("[redacted]"), "key must be redacted");
    assert!(!debugged.contains("enc-secret"), "key leaked: {debugged}");
    let input = NewProvider {
        name: "secretive".to_string(),
        api_key_encrypted: Some("new-enc-secret".to_string()),
        ..NewProvider::default()
    };
    let patch = ProviderPatch {
        api_key_encrypted: Some("patch-enc-secret".to_string()),
        ..ProviderPatch::default()
    };
    for rendered in [format!("{input:?}"), format!("{patch:?}")] {
        assert!(
            rendered.contains("[redacted]"),
            "input key must be redacted"
        );
        assert!(!rendered.contains("enc-secret"), "input leaked: {rendered}");
    }

    let live = store.list_providers("local-user", false).expect("live");
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].id, active.id);
    let all = store.list_providers("local-user", true).expect("all");
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].id, active.id);
    assert_eq!(all[1].id, inactive.id);

    let patched = store
        .update_provider(
            "local-user",
            active.id,
            &ProviderPatch {
                label: Some("Renamed".to_string()),
                is_active: Some(false),
                chat_models: Some(json!(["gpt-4o", "gpt-4o-mini"])),
                ..ProviderPatch::default()
            },
        )
        .expect("patch");
    assert_eq!(patched.label.as_deref(), Some("Renamed"));
    assert!(!patched.is_active);
    assert_eq!(patched.chat_models, Some(json!(["gpt-4o", "gpt-4o-mini"])));
    assert!(
        store
            .list_providers("local-user", false)
            .expect("live again")
            .is_empty()
    );

    store
        .delete_provider("local-user", active.id)
        .expect("delete");
    assert!(matches!(
        store.get_provider("local-user", active.id),
        Err(StoreError::NotFound("provider"))
    ));
}

#[test]
fn setting_the_default_provider_is_mutually_exclusive() {
    let (_directory, store) = adopted_store();
    let first = store
        .create_provider("local-user", &provider("openai", true))
        .expect("first");
    let second = store
        .create_provider("local-user", &provider("deepseek", true))
        .expect("second");
    let fallback = store
        .create_provider(
            "local-user",
            &NewProvider {
                is_fallback: true,
                ..provider("backup", true)
            },
        )
        .expect("fallback");

    assert_eq!(
        store
            .default_provider("local-user")
            .expect("default")
            .expect("some")
            .id,
        first.id,
        "first active non-fallback wins with no explicit default"
    );

    let defaulted = store
        .set_default_provider("local-user", second.id)
        .expect("set default");
    assert!(defaulted.is_default);
    assert!(
        !store
            .get_provider("local-user", first.id)
            .expect("first")
            .is_default,
        "other defaults cleared"
    );
    assert_eq!(
        store
            .default_provider("local-user")
            .expect("default")
            .expect("some")
            .id,
        second.id
    );

    let patched = store
        .update_provider(
            "local-user",
            first.id,
            &ProviderPatch {
                is_default: Some(true),
                ..ProviderPatch::default()
            },
        )
        .expect("patch default");
    assert!(patched.is_default);
    assert!(
        !store
            .get_provider("local-user", second.id)
            .expect("second")
            .is_default
    );

    store
        .set_default_provider("local-user", fallback.id)
        .expect("fallback default");
    assert_eq!(
        store
            .default_provider("local-user")
            .expect("default")
            .expect("some")
            .id,
        first.id,
        "default selection skips fallback rows"
    );
}

#[test]
fn providers_are_actor_scoped() {
    let (_directory, store) = adopted_store();
    let mine = store
        .create_provider("local-user", &provider("openai", true))
        .expect("create");
    store.ensure_actor("other-actor").expect("actor");

    assert!(
        store
            .list_providers("other-actor", true)
            .expect("list")
            .is_empty()
    );
    assert!(
        store
            .default_provider("other-actor")
            .expect("default")
            .is_none()
    );
    assert!(matches!(
        store.get_provider("other-actor", mine.id),
        Err(StoreError::NotFound("provider"))
    ));
    assert!(matches!(
        store.delete_provider("other-actor", mine.id),
        Err(StoreError::NotFound("provider"))
    ));
    assert!(matches!(
        store.set_default_provider("other-actor", mine.id),
        Err(StoreError::NotFound("provider"))
    ));
    assert!(matches!(
        store.update_provider(
            "other-actor",
            mine.id,
            &ProviderPatch {
                label: Some("hax".to_string()),
                ..ProviderPatch::default()
            }
        ),
        Err(StoreError::NotFound("provider"))
    ));
}
