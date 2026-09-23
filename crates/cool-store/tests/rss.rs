//! RSS subscription/entry store parity tests.

mod common;

use common::{DEFAULT_SEED, create_python_database, temp_database};
use cool_store::domains::rss::{NewRssEntry, NewRssSubscription};
use cool_store::{LegacyStore, StoreError};
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

fn subscription(url: &str) -> NewRssSubscription {
    NewRssSubscription {
        url: url.to_string(),
        ..NewRssSubscription::default()
    }
}

fn entry(guid: &str) -> NewRssEntry {
    NewRssEntry {
        guid: guid.to_string(),
        title: Some(format!("title {guid}")),
        ..NewRssEntry::default()
    }
}

#[test]
fn subscriptions_apply_defaults_clamp_and_ordering() {
    let (directory, store) = adopted_store();

    let first = store
        .create_subscription(
            "local-user",
            &NewRssSubscription {
                category: Some("news".to_string()),
                ..subscription("https://example.com/a.xml")
            },
        )
        .expect("create first");
    assert_eq!(first.user_id, 1);
    assert_eq!(first.fetch_interval_minutes, 60);
    assert!(first.enabled);
    assert_eq!(first.entry_count, 0);

    let second = store
        .create_subscription(
            "local-user",
            &NewRssSubscription {
                fetch_interval_minutes: Some(1),
                enabled: Some(false),
                ..subscription("https://example.com/b.xml")
            },
        )
        .expect("create second");
    assert_eq!(
        second.fetch_interval_minutes, 5,
        "clamped to a 5 minute floor"
    );
    assert!(!second.enabled);

    let all = store
        .list_subscriptions("local-user", None, None)
        .expect("list");
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].id, second.id, "newest first");
    assert_eq!(all[1].id, first.id);

    let enabled = store
        .list_subscriptions("local-user", None, Some(true))
        .expect("enabled");
    assert_eq!(enabled.len(), 1);
    assert_eq!(enabled[0].id, first.id);

    let by_category = store
        .list_subscriptions("local-user", Some("news"), None)
        .expect("category");
    assert_eq!(by_category.len(), 1);
    assert_eq!(by_category[0].id, first.id);

    let duplicate =
        store.create_subscription("local-user", &subscription("https://example.com/a.xml"));
    assert!(matches!(duplicate, Err(StoreError::Conflict(_))));

    store
        .insert_entry_if_new("local-user", first.id, &entry("g1"))
        .expect("insert");
    store
        .record_fetch_result(
            "local-user",
            first.id,
            "2026-05-01 00:00:00.000000",
            Some("timeout"),
        )
        .expect("record fetch");
    let refreshed = store.get_subscription("local-user", first.id).expect("get");
    // A failure never touches entry_count (Python parity).
    assert_eq!(
        refreshed.entry_count, 1,
        "failure must not zero entry_count"
    );
    assert_eq!(refreshed.last_error.as_deref(), Some("timeout"));
    assert_eq!(
        refreshed.last_fetched_at.as_deref(),
        Some("2026-05-01 00:00:00.000000")
    );

    store
        .insert_entry_if_new("local-user", first.id, &entry("g2"))
        .expect("insert");
    // Drift the denormalized counter so the success path must self-heal it.
    {
        use rusqlite::Connection;
        let connection = Connection::open(directory.path().join("harness.db")).expect("open");
        connection
            .execute(
                "UPDATE rss_subscriptions SET entry_count = 99 WHERE id = ?1",
                [first.id],
            )
            .expect("drift");
    }
    store
        .record_fetch_result("local-user", first.id, "2026-05-01 00:00:00.000000", None)
        .expect("record success");
    let refreshed = store.get_subscription("local-user", first.id).expect("get");
    assert_eq!(
        refreshed.entry_count, 2,
        "success recounts entry_count to COUNT(*)"
    );
    assert_eq!(refreshed.last_error, None);

    store
        .delete_subscription("local-user", second.id)
        .expect("delete");
    let gone = store
        .get_subscription("local-user", second.id)
        .expect_err("gone");
    assert!(matches!(gone, StoreError::NotFound("rss subscription")));
}

#[test]
fn entries_dedup_by_guid_and_order_newest_first() {
    let (_directory, store) = adopted_store();
    let sub = store
        .create_subscription("local-user", &subscription("https://example.com/feed.xml"))
        .expect("create");

    let first = store
        .insert_entry_if_new(
            "local-user",
            sub.id,
            &NewRssEntry {
                published_at: Some("2026-01-01 00:00:00.000000".to_string()),
                ..entry("g1")
            },
        )
        .expect("insert")
        .expect("new");
    let second = store
        .insert_entry_if_new(
            "local-user",
            sub.id,
            &NewRssEntry {
                published_at: None,
                ..entry("g2")
            },
        )
        .expect("insert")
        .expect("new");
    let third = store
        .insert_entry_if_new(
            "local-user",
            sub.id,
            &NewRssEntry {
                published_at: Some("2026-02-01 00:00:00.000000".to_string()),
                ..entry("g3")
            },
        )
        .expect("insert")
        .expect("new");

    let duplicate = store
        .insert_entry_if_new("local-user", sub.id, &entry("g1"))
        .expect("duplicate");
    assert!(duplicate.is_none(), "same guid must not insert twice");

    let count = store
        .get_subscription("local-user", sub.id)
        .expect("get")
        .entry_count;
    assert_eq!(count, 3, "entry_count bumps only on real inserts");

    let ordered = store
        .list_entries("local-user", sub.id, None, false)
        .expect("list");
    assert_eq!(
        ordered.iter().map(|row| row.id).collect::<Vec<_>>(),
        vec![third.id, first.id, second.id],
        "published_at DESC with NULLS LAST and stable id tiebreak"
    );
    assert!(!first.is_read);

    store
        .mark_entry_read("local-user", first.id, true)
        .expect("mark read");
    let unread = store
        .list_entries("local-user", sub.id, None, true)
        .expect("unread");
    assert_eq!(
        unread.iter().map(|row| row.id).collect::<Vec<_>>(),
        vec![third.id, second.id]
    );

    let all = store
        .list_all_entries("local-user", None, false)
        .expect("all");
    assert_eq!(all.len(), 3);

    let reloaded = store
        .mark_entry_read("local-user", first.id, false)
        .expect("mark unread");
    assert!(!reloaded.is_read);
}

#[test]
fn rss_operations_are_actor_scoped() {
    let (_directory, store) = adopted_store();
    let sub = store
        .create_subscription("local-user", &subscription("https://example.com/feed.xml"))
        .expect("create");
    let stored = store
        .insert_entry_if_new("local-user", sub.id, &entry("g1"))
        .expect("insert")
        .expect("new");

    store.ensure_actor("other-actor").expect("register actor");

    assert!(
        store
            .list_subscriptions("other-actor", None, None)
            .expect("list")
            .is_empty()
    );
    assert!(matches!(
        store.get_subscription("other-actor", sub.id),
        Err(StoreError::NotFound("rss subscription"))
    ));
    assert!(matches!(
        store.insert_entry_if_new("other-actor", sub.id, &entry("g2")),
        Err(StoreError::NotFound("rss subscription"))
    ));
    assert!(matches!(
        store.mark_entry_read("other-actor", stored.id, true),
        Err(StoreError::NotFound("rss entry"))
    ));
    assert!(
        store
            .list_all_entries("other-actor", None, false)
            .expect("all")
            .is_empty()
    );
}

#[test]
fn fill_subscription_meta_only_backfills_missing_fields() {
    let (_directory, store) = adopted_store();
    let sub = store
        .create_subscription(
            "local-user",
            &NewRssSubscription {
                title: Some("kept".to_string()),
                ..subscription("https://example.com/feed.xml")
            },
        )
        .expect("create");

    store
        .fill_subscription_meta(
            "local-user",
            sub.id,
            Some("ignored"),
            Some("https://example.com/"),
        )
        .expect("fill");
    let refreshed = store.get_subscription("local-user", sub.id).expect("get");
    assert_eq!(refreshed.title.as_deref(), Some("kept"));
    assert_eq!(refreshed.site_url.as_deref(), Some("https://example.com/"));

    store
        .fill_subscription_meta("local-user", sub.id, Some("later"), Some("https://other/"))
        .expect("fill again");
    let refreshed = store.get_subscription("local-user", sub.id).expect("get");
    assert_eq!(refreshed.title.as_deref(), Some("kept"));
    assert_eq!(refreshed.site_url.as_deref(), Some("https://example.com/"));
}

#[test]
fn prune_keeps_newest_entries_and_resets_the_count() {
    let (_directory, store) = adopted_store();
    let sub = store
        .create_subscription("local-user", &subscription("https://example.com/feed.xml"))
        .expect("create");

    for index in 0..5 {
        let guid = format!("g{index}");
        store
            .insert_entry_if_new(
                "local-user",
                sub.id,
                &NewRssEntry {
                    published_at: Some(format!("2026-01-0{index} 00:00:00.000000")),
                    ..entry(&guid)
                },
            )
            .expect("insert");
    }
    assert_eq!(store.count_entries("local-user", sub.id).expect("count"), 5);

    let removed = store
        .prune_subscription_entries("local-user", sub.id, 3)
        .expect("prune");
    assert_eq!(removed, 2);
    let remaining = store
        .list_entries("local-user", sub.id, None, false)
        .expect("list");
    assert_eq!(remaining.len(), 3);
    // Newest three survive (2026-01-02..04); the two oldest are gone.
    let guids: Vec<_> = remaining.iter().map(|row| row.guid.as_str()).collect();
    assert_eq!(guids, vec!["g4", "g3", "g2"]);
    let refreshed = store.get_subscription("local-user", sub.id).expect("get");
    assert_eq!(refreshed.entry_count, 3);

    let none = store
        .prune_subscription_entries("local-user", sub.id, 3)
        .expect("prune again");
    assert_eq!(none, 0);
}

#[test]
fn prune_treats_a_null_published_at_as_the_fetch_time() {
    let (_directory, store) = adopted_store();
    let sub = store
        .create_subscription("local-user", &subscription("https://example.com/feed.xml"))
        .expect("create");
    // g1 has no published_at, so it sorts by fetched_at (now) and is the newest.
    store
        .insert_entry_if_new("local-user", sub.id, &entry("g1"))
        .expect("insert");
    for index in 0..3 {
        store
            .insert_entry_if_new(
                "local-user",
                sub.id,
                &NewRssEntry {
                    published_at: Some("2020-01-01 00:00:00.000000".to_string()),
                    ..entry(&format!("old{index}"))
                },
            )
            .expect("insert");
    }
    let removed = store
        .prune_subscription_entries("local-user", sub.id, 2)
        .expect("prune");
    assert_eq!(removed, 2);
    let remaining = store
        .list_entries("local-user", sub.id, None, false)
        .expect("list");
    // g1 (null published_at → sorts as fetched_at = now) survives the prune as
    // the newest; the two oldest published rows are deleted. `list_entries`
    // itself is NULLS LAST, so g1 appears last even though it is newest.
    let mut guids: Vec<_> = remaining.iter().map(|row| row.guid.as_str()).collect();
    guids.sort_unstable();
    assert_eq!(guids, vec!["g1", "old2"]);
}
