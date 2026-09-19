//! Memory records CRUD parity tests.

mod common;

use common::{DEFAULT_SEED, create_python_database, temp_database};
use cool_store::StoreError;
use cool_store::domains::memory::{
    EntityPatch, MemoryFilter, MemoryItemPatch, NewEntity, NewEpisode, NewMemoryItem, NewRelation,
};
use cool_store::{LegacyStore, StoreOptions};
use serde_json::json;
use tempfile::TempDir;

fn adopted_store() -> (TempDir, LegacyStore) {
    let directory = TempDir::new().expect("tempdir");
    let path = temp_database(directory.path(), "harness.db");
    create_python_database(&path, DEFAULT_SEED);
    let store = LegacyStore::open(
        &path,
        &StoreOptions {
            initialize_if_missing: false,
            ..StoreOptions::default()
        },
    )
    .expect("adopt");
    (directory, store)
}

fn new_item(content: &str) -> NewMemoryItem {
    NewMemoryItem {
        content: content.to_string(),
        ..NewMemoryItem::default()
    }
}

#[test]
fn memory_item_crud_defaults_and_lifecycle_flags() {
    let (_directory, store) = adopted_store();

    let created = store
        .create_memory_item(
            "local-user",
            &NewMemoryItem {
                content: "The sky is blue".to_string(),
                tags: Some(json!(["facts"])),
                structured: Some(json!({"color": "blue"})),
                ..NewMemoryItem::default()
            },
        )
        .expect("create");
    assert_eq!(created.status, "pending_confirmation");
    assert_eq!(created.memory_type, "semantic");
    assert_eq!(created.scope, "global");
    assert!(!created.pinned);

    let fetched = store
        .get_memory_item("local-user", created.id)
        .expect("get");
    assert_eq!(fetched, created);

    let updated = store
        .update_memory_item(
            "local-user",
            created.id,
            &MemoryItemPatch {
                content: Some("The sky is cerulean".to_string()),
                importance: Some(0.8),
                ..MemoryItemPatch::default()
            },
        )
        .expect("update");
    assert_eq!(updated.content, "The sky is cerulean");
    assert_eq!(updated.importance, 0.8);

    let pending = store
        .list_memory_items(
            "local-user",
            &MemoryFilter {
                status: Some("pending_confirmation".to_string()),
                ..MemoryFilter::default()
            },
        )
        .expect("list pending");
    assert_eq!(pending.len(), 1);

    let confirmed = store
        .confirm_memory_item("local-user", created.id)
        .expect("confirm");
    assert_eq!(confirmed.status, "active");

    let pinned = store
        .pin_memory_item("local-user", created.id, true)
        .expect("pin");
    assert!(pinned.pinned);

    let active = store
        .list_memory_items(
            "local-user",
            &MemoryFilter {
                status: Some("active".to_string()),
                pinned: Some(true),
                ..MemoryFilter::default()
            },
        )
        .expect("list active");
    assert_eq!(active.len(), 1);

    store
        .delete_memory_item("local-user", created.id, false)
        .expect("soft delete");
    assert_eq!(
        store
            .get_memory_item("local-user", created.id)
            .expect("archived")
            .status,
        "archived"
    );

    store
        .delete_memory_item("local-user", created.id, true)
        .expect("hard delete");
    let error = store
        .get_memory_item("local-user", created.id)
        .expect_err("gone");
    assert!(matches!(error, StoreError::NotFound("memory item")));
}

#[test]
fn memory_listing_filters_stats_and_actor_scope() {
    let (_directory, store) = adopted_store();
    store
        .create_memory_item(
            "local-user",
            &NewMemoryItem {
                content: "global preference".to_string(),
                memory_type: "preference".to_string(),
                source: "user_explicit".to_string(),
                importance: 0.9,
                ..NewMemoryItem::default()
            },
        )
        .expect("preference");
    store
        .create_memory_item(
            "local-user",
            &NewMemoryItem {
                content: "conversation fact".to_string(),
                scope: "conversation".to_string(),
                conversation_id: Some(1),
                source: "user_explicit".to_string(),
                ..NewMemoryItem::default()
            },
        )
        .expect("conversation memory");

    let preferences = store
        .list_memory_items(
            "local-user",
            &MemoryFilter {
                memory_type: Some("preference".to_string()),
                ..MemoryFilter::default()
            },
        )
        .expect("by type");
    assert_eq!(preferences.len(), 1);

    let scoped = store
        .list_memory_items(
            "local-user",
            &MemoryFilter {
                conversation_id: Some(1),
                ..MemoryFilter::default()
            },
        )
        .expect("by conversation");
    assert_eq!(scoped.len(), 1);
    // The default fixture conversation has no working directory, so no
    // `_project_key` is attached (Python `_attach_project_key` convention).
    assert_eq!(scoped[0].structured, None);

    let stats = store.memory_stats("local-user").expect("stats");
    assert_eq!(stats.total_active, 2);
    assert_eq!(stats.by_type.get("preference"), Some(&1));
    assert_eq!(stats.by_scope.get("conversation"), Some(&1));
    assert_eq!(stats.total_episodes, 0);
    assert_eq!(stats.total_archived, 0);
    assert_eq!(stats.total_pending, 0);
    assert_eq!(stats.total_entities, 0);

    store.ensure_actor("other-actor").expect("actor");
    let error = store
        .get_memory_item("other-actor", preferences[0].id)
        .expect_err("cross-actor");
    assert!(matches!(error, StoreError::NotFound("memory item")));
    assert!(
        store
            .list_memory_items("other-actor", &MemoryFilter::default())
            .expect("list other")
            .is_empty()
    );
}

#[test]
fn agent_importance_is_capped_and_confirm_archives_superseded() {
    let (_directory, store) = adopted_store();
    let old = store
        .create_memory_item(
            "local-user",
            &NewMemoryItem {
                content: "the server runs on port 8080".to_string(),
                source: "user_explicit".to_string(),
                ..NewMemoryItem::default()
            },
        )
        .expect("old");

    let new = store
        .create_memory_item(
            "local-user",
            &NewMemoryItem {
                content: "the server runs on port 9090".to_string(),
                importance: 0.99,
                supersedes_id: Some(old.id),
                ..NewMemoryItem::default()
            },
        )
        .expect("new");
    assert_eq!(new.status, "pending_confirmation");
    assert_eq!(new.importance, 0.9, "agent importance is capped at 0.9");

    store
        .confirm_memory_item("local-user", new.id)
        .expect("confirm");
    let old_after = store.get_memory_item("local-user", old.id).expect("old");
    assert_eq!(old_after.status, "superseded");
    assert_eq!(old_after.supersedes_id, Some(new.id));

    let rejected = store
        .create_memory_item("local-user", &new_item("reject me"))
        .expect("pending");
    assert_eq!(
        store
            .reject_memory_item("local-user", rejected.id)
            .expect("reject")
            .status,
        "archived"
    );
    assert!(
        store
            .list_pending_items("local-user", None)
            .expect("pending")
            .is_empty()
    );
}

#[test]
fn episodes_and_working_memory_round_trip() {
    let (_directory, store) = adopted_store();
    let episode = store
        .create_episode(
            "local-user",
            &NewEpisode {
                conversation_id: Some(1),
                title: "Fixed the build".to_string(),
                summary: "Re-ran cargo build after clearing the cache.".to_string(),
                outcome: "success".to_string(),
                importance: 0.7,
                ..NewEpisode::default()
            },
        )
        .expect("create episode");
    assert_eq!(episode.outcome, "success");
    assert_eq!(
        store.get_episode("local-user", episode.id).expect("get"),
        episode
    );
    assert_eq!(
        store.list_episodes("local-user", None).expect("list").len(),
        1
    );

    assert!(
        store
            .get_working_memory("local-user", 1)
            .expect("none")
            .is_none()
    );
    let working = store
        .upsert_working_memory(
            "local-user",
            1,
            &json!({"goal": "ship"}),
            Some("summary"),
            Some(3),
            Some(42),
        )
        .expect("create working");
    assert_eq!(working.state, json!({"goal": "ship"}));
    assert_eq!(working.summary.as_deref(), Some("summary"));
    assert_eq!(working.summary_up_to_message_id, Some(3));
    assert_eq!(working.token_estimate, Some(42));

    let updated = store
        .upsert_working_memory(
            "local-user",
            1,
            &json!({"goal": "ship v2"}),
            None,
            None,
            Some(50),
        )
        .expect("update working");
    assert_eq!(updated.id, working.id);
    assert_eq!(updated.state, json!({"goal": "ship v2"}));
    assert_eq!(updated.summary, None);
    assert_eq!(updated.token_estimate, Some(50));
}

#[test]
fn entities_relations_and_links_are_actor_scoped() {
    let (_directory, store) = adopted_store();
    let person = store
        .create_entity(
            "local-user",
            &NewEntity {
                name: "Ada".to_string(),
                entity_type: "person".to_string(),
                aliases: Some(json!(["Ada Lovelace"])),
                attributes: Some(json!({"role": "engineer"})),
                ..NewEntity::default()
            },
        )
        .expect("person");
    let project = store
        .create_entity(
            "local-user",
            &NewEntity {
                name: "Harness".to_string(),
                entity_type: "project".to_string(),
                ..NewEntity::default()
            },
        )
        .expect("project");

    let conflict = store
        .create_entity(
            "local-user",
            &NewEntity {
                name: "Ada".to_string(),
                ..NewEntity::default()
            },
        )
        .expect_err("unique name");
    assert!(matches!(conflict, StoreError::Conflict(_)));

    let updated = store
        .update_entity(
            "local-user",
            person.id,
            &EntityPatch {
                description: Some("Pioneer".to_string()),
                ..EntityPatch::default()
            },
        )
        .expect("update");
    assert_eq!(updated.description.as_deref(), Some("Pioneer"));

    let by_alias = store
        .list_entities("local-user", Some("lovelace"), None, None)
        .expect("alias search");
    assert_eq!(by_alias.len(), 1);
    assert_eq!(by_alias[0].id, person.id);
    let by_type = store
        .list_entities("local-user", None, Some("project"), None)
        .expect("type search");
    assert_eq!(by_type.len(), 1);
    assert_eq!(by_type[0].id, project.id);

    let memory = store
        .create_memory_item(
            "local-user",
            &NewMemoryItem {
                content: "Ada works on Harness".to_string(),
                source: "user_explicit".to_string(),
                ..NewMemoryItem::default()
            },
        )
        .expect("memory");
    assert!(
        store
            .link_memory_entity("local-user", memory.id, person.id)
            .expect("link")
    );
    assert!(
        !store
            .link_memory_entity("local-user", memory.id, person.id)
            .expect("idempotent")
    );
    assert_eq!(
        store
            .list_memory_entities("local-user", memory.id)
            .expect("memory entities")
            .len(),
        1
    );
    assert_eq!(
        store
            .list_entity_memories("local-user", person.id, None)
            .expect("entity memories")
            .len(),
        1
    );

    let relation = store
        .create_relation(
            "local-user",
            &NewRelation {
                source_entity_id: person.id,
                target_entity_id: project.id,
                relation_type: "works_on".to_string(),
                ..NewRelation::default()
            },
        )
        .expect("relation");
    let same = store
        .create_relation(
            "local-user",
            &NewRelation {
                source_entity_id: person.id,
                target_entity_id: project.id,
                relation_type: "works_on".to_string(),
                ..NewRelation::default()
            },
        )
        .expect("idempotent relation");
    assert_eq!(same.id, relation.id);
    assert_eq!(
        store
            .list_relations("local-user", person.id)
            .expect("relations")
            .len(),
        1
    );

    assert!(
        store
            .unlink_memory_entity("local-user", memory.id, person.id)
            .expect("unlink")
    );
    store
        .delete_relation("local-user", relation.id)
        .expect("delete relation");
    assert!(
        store
            .list_relations("local-user", person.id)
            .expect("none")
            .is_empty()
    );

    // Deleting the entity cascades to remaining links/relations.
    store
        .link_memory_entity("local-user", memory.id, person.id)
        .expect("relink");
    store
        .delete_entity("local-user", person.id)
        .expect("delete entity");
    let error = store.get_entity("local-user", person.id).expect_err("gone");
    assert!(matches!(error, StoreError::NotFound("entity")));
    assert!(
        store
            .list_memory_entities("local-user", memory.id)
            .expect("links cleaned")
            .is_empty()
    );
}

#[test]
fn embedding_bookkeeping_tracks_missing_active_memories() {
    let (_directory, store) = adopted_store();
    let first = store
        .create_memory_item("local-user", &new_item("first"))
        .expect("first");
    let second = store
        .create_memory_item(
            "local-user",
            &NewMemoryItem {
                content: "second".to_string(),
                source: "user_explicit".to_string(),
                ..NewMemoryItem::default()
            },
        )
        .expect("second");

    let missing = store
        .list_memories_missing_embeddings("local-user", None)
        .expect("missing");
    assert_eq!(
        missing,
        vec![second.id],
        "pending item is not a backfill target"
    );

    store
        .record_memory_embedding("local-user", second.id, "text-embedding-3-small", 1536)
        .expect("record");
    store
        .record_memory_embedding("local-user", second.id, "text-embedding-3-large", 3072)
        .expect("upsert");
    assert!(
        store
            .list_memories_missing_embeddings("local-user", None)
            .expect("missing after")
            .is_empty()
    );
    assert_eq!(
        cool_store::domains::memory::memory_embedding_dimension(),
        1536
    );

    // Pending item is not an embedding target; confirm it to make it eligible.
    store
        .confirm_memory_item("local-user", first.id)
        .expect("confirm");
    assert_eq!(
        store
            .list_memories_missing_embeddings("local-user", None)
            .expect("first missing"),
        vec![first.id]
    );
    assert!(
        store
            .delete_memory_embedding("local-user", second.id)
            .expect("delete embedding")
    );
    assert!(
        !store
            .delete_memory_embedding("local-user", second.id)
            .expect("already deleted")
    );
}
