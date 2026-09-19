//! Webhook endpoint/event store parity tests.

mod common;

use common::{DEFAULT_SEED, create_python_database, temp_database};
use cool_store::domains::webhooks::{NewWebhookEndpoint, NewWebhookEvent, WebhookEndpointPatch};
use cool_store::{LegacyStore, StoreError};
use serde_json::json;
use tempfile::TempDir;

/// Seed a scheduled task and a task run so events can reference a real
/// `task_run_id` (the legacy FK is enforced by the bundled SQLite build).
const TASK_SEED: &str = "
INSERT INTO scheduled_tasks(created_at, updated_at, id, user_id, name, trigger_type, timezone,
  misfire_policy, prompt, approval_policy, max_iterations, enabled, run_count, failure_count)
VALUES ('2026-01-01 00:00:00.000000', '2026-01-01 00:00:00.000000', 1, 1, 'Task', 'interval',
  'UTC', 'run', 'do work', 'auto', 10, 1, 0, 0);
INSERT INTO task_runs(created_at, updated_at, id, task_id, status, trigger_source, prompt,
  is_read, started_at)
VALUES ('2026-01-01 00:00:00.000000', '2026-01-01 00:00:00.000000', 1, 1, 'running', 'manual',
  'do work', 0, '2026-01-01 00:00:00.000000');
";

fn adopted_store() -> (TempDir, LegacyStore) {
    let directory = TempDir::new().expect("tempdir");
    let path = temp_database(directory.path(), "harness.db");
    create_python_database(&path, &format!("{DEFAULT_SEED}\n{TASK_SEED}"));
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

fn endpoint(name: &str, source_type: &str) -> NewWebhookEndpoint {
    NewWebhookEndpoint {
        name: name.to_string(),
        source_type: source_type.to_string(),
        ..NewWebhookEndpoint::default()
    }
}

#[test]
fn endpoint_crud_generates_ids_and_patches() {
    let (_directory, store) = adopted_store();
    let created = store
        .create_endpoint(
            "local-user",
            &NewWebhookEndpoint {
                event_filter: Some(json!(["push"])),
                ..endpoint("GitHub", "github")
            },
        )
        .expect("create");

    assert_eq!(created.hook_id.len(), 32, "hyphenless uuid hex");
    assert!(!created.hook_id.contains('-'));
    assert_eq!(created.secret.len(), 64, "32 random bytes hex-encoded");
    assert!(created.enabled);
    assert_eq!(created.user_id, 1);

    // Debug output must not leak the HMAC secret.
    let debugged = format!("{created:?}");
    assert!(debugged.contains("[redacted]"), "secret must be redacted");
    assert!(
        !debugged.contains(&created.secret),
        "secret leaked: {debugged}"
    );
    let input = NewWebhookEndpoint {
        secret: Some("new-hmac-secret".to_string()),
        ..endpoint("Input", "custom")
    };
    let rendered = format!("{input:?}");
    assert!(
        rendered.contains("[redacted]"),
        "input secret must be redacted"
    );
    assert!(
        !rendered.contains("new-hmac-secret"),
        "input leaked: {rendered}"
    );

    let found = store
        .find_endpoint_by_hook_id(&created.hook_id)
        .expect("lookup")
        .expect("present");
    assert_eq!(found.id, created.id);

    assert!(
        store
            .find_endpoint_by_hook_id("missing-hook")
            .expect("lookup")
            .is_none()
    );

    let updated = store
        .update_endpoint(
            "local-user",
            created.id,
            &WebhookEndpointPatch {
                name: Some("Renamed".to_string()),
                enabled: Some(false),
                event_filter: Some(json!(["push", "pull_request"])),
                ..WebhookEndpointPatch::default()
            },
        )
        .expect("update");
    assert_eq!(updated.name, "Renamed");
    assert!(!updated.enabled);
    assert_eq!(updated.event_filter, Some(json!(["push", "pull_request"])));

    let unknown_source = store.update_endpoint(
        "local-user",
        created.id,
        &WebhookEndpointPatch {
            source_type: Some("bogus".to_string()),
            ..WebhookEndpointPatch::default()
        },
    );
    assert!(matches!(unknown_source, Err(StoreError::InvalidInput(_))));

    let explicit = store
        .create_endpoint(
            "local-user",
            &NewWebhookEndpoint {
                hook_id: Some("fixed-hook".to_string()),
                secret: Some("fixed-secret".to_string()),
                ..endpoint("Custom", "custom")
            },
        )
        .expect("create explicit");
    assert_eq!(explicit.hook_id, "fixed-hook");
    assert_eq!(explicit.secret, "fixed-secret");

    let listed = store.list_endpoints("local-user").expect("list");
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].id, explicit.id, "newest first");
}

#[test]
fn hook_id_is_unique() {
    let (_directory, store) = adopted_store();
    store
        .create_endpoint(
            "local-user",
            &NewWebhookEndpoint {
                hook_id: Some("dup-hook".to_string()),
                ..endpoint("One", "custom")
            },
        )
        .expect("first");
    let duplicate = store.create_endpoint(
        "local-user",
        &NewWebhookEndpoint {
            hook_id: Some("dup-hook".to_string()),
            ..endpoint("Two", "custom")
        },
    );
    assert!(matches!(duplicate, Err(StoreError::Sqlite(_))));
}

#[test]
fn events_record_update_and_delete_with_endpoint() {
    let (_directory, store) = adopted_store();
    let ep = store
        .create_endpoint("local-user", &endpoint("Receiver", "custom"))
        .expect("create");

    let event = store
        .record_webhook_event(
            ep.id,
            &NewWebhookEvent {
                event_type: Some("push".to_string()),
                payload: Some(json!({"ref": "main"})),
                signature_valid: true,
                status: None,
            },
        )
        .expect("record");
    assert_eq!(event.status, "received", "status defaults to received");
    assert!(event.signature_valid);
    assert!(!event.received_at.is_empty());

    store
        .update_webhook_event(event.id, "completed", None, Some(1))
        .expect("update event");
    let events = store
        .list_webhook_events("local-user", ep.id, None)
        .expect("list");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].status, "completed");
    assert_eq!(events[0].task_run_id, Some(1));

    let rejected = store
        .record_webhook_event(
            ep.id,
            &NewWebhookEvent {
                signature_valid: false,
                status: Some("rejected"),
                ..NewWebhookEvent::default()
            },
        )
        .expect("record rejected");
    assert_eq!(rejected.status, "rejected");
    assert!(!rejected.signature_valid);

    let newest_first = store
        .list_webhook_events("local-user", ep.id, None)
        .expect("list");
    assert_eq!(newest_first[0].id, rejected.id);

    store
        .update_endpoint("local-user", ep.id, &WebhookEndpointPatch::default())
        .expect("noop update");

    store
        .delete_endpoint("local-user", ep.id)
        .expect("delete endpoint");
    assert!(matches!(
        store.get_webhook_event("local-user", event.id),
        Err(StoreError::NotFound("webhook event"))
    ));
}

#[test]
fn endpoint_management_is_actor_scoped_but_inbound_is_not() {
    let (_directory, store) = adopted_store();
    let ep = store
        .create_endpoint("local-user", &endpoint("Receiver", "custom"))
        .expect("create");
    let event = store
        .record_webhook_event(
            ep.id,
            &NewWebhookEvent {
                signature_valid: true,
                ..NewWebhookEvent::default()
            },
        )
        .expect("record");

    store.ensure_actor("other-actor").expect("register");
    assert!(
        store
            .list_endpoints("other-actor")
            .expect("list")
            .is_empty()
    );
    assert!(matches!(
        store.get_endpoint("other-actor", ep.id),
        Err(StoreError::NotFound("webhook endpoint"))
    ));
    assert!(matches!(
        store.list_webhook_events("other-actor", ep.id, None),
        Err(StoreError::NotFound("webhook endpoint"))
    ));
    assert!(matches!(
        store.get_webhook_event("other-actor", event.id),
        Err(StoreError::NotFound("webhook event"))
    ));

    // Inbound path has no actor: lookup by hook_id and event recording still
    // work without one.
    assert!(
        store
            .find_endpoint_by_hook_id(&ep.hook_id)
            .expect("lookup")
            .is_some()
    );
    store
        .record_webhook_event(
            ep.id,
            &NewWebhookEvent {
                signature_valid: true,
                ..NewWebhookEvent::default()
            },
        )
        .expect("inbound record");
}
