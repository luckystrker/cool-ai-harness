//! Wiki article store parity tests.

mod common;

use common::{DEFAULT_SEED, create_python_database, temp_database};
use cool_store::domains::wiki::{NewWikiArticle, WikiArticlePatch, WikiFilter};
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

fn article(title: &str, content: &str, category: &str, tags: &[&str]) -> NewWikiArticle {
    NewWikiArticle {
        title: title.to_string(),
        content: content.to_string(),
        category: category.to_string(),
        tags: Some(json!(tags)),
        source: "manual".to_string(),
        ..NewWikiArticle::default()
    }
}

#[test]
fn article_crud_bumps_version_only_on_content_change() {
    let (_directory, store) = adopted_store();
    let created = store
        .create_article(
            "local-user",
            &NewWikiArticle {
                tags: Some(json!(["rust"])),
                source: "agent".to_string(),
                project_key: Some("proj".to_string()),
                ..article("Rust Ownership", "v1 body", "dev", &["rust"])
            },
        )
        .expect("create");
    assert_eq!(created.version, 1);
    assert_eq!(created.user_id, Some(1));
    assert!(!created.is_pinned);
    assert!(!created.is_archived);
    assert_eq!(created.tags, Some(json!(["rust"])));
    assert_eq!(created.source, "agent");
    assert_eq!(created.project_key.as_deref(), Some("proj"));

    let updated = store
        .update_article(
            "local-user",
            created.id,
            &WikiArticlePatch {
                content: Some("v2 body".to_string()),
                ..WikiArticlePatch::default()
            },
        )
        .expect("update content");
    assert_eq!(updated.content, "v2 body");
    assert_eq!(updated.version, 2);

    let title_only = store
        .update_article(
            "local-user",
            created.id,
            &WikiArticlePatch {
                title: Some("Renamed".to_string()),
                ..WikiArticlePatch::default()
            },
        )
        .expect("update title");
    assert_eq!(title_only.title, "Renamed");
    assert_eq!(title_only.version, 2, "title change does not bump version");

    let got = store.get_article("local-user", created.id).expect("get");
    assert_eq!(got.title, "Renamed");

    store
        .delete_article("local-user", created.id)
        .expect("delete");
    assert!(matches!(
        store.get_article("local-user", created.id),
        Err(StoreError::NotFound("wiki article"))
    ));
}

#[test]
fn listing_pins_then_orders_and_filters() {
    let (_directory, store) = adopted_store();
    let first = store
        .create_article("local-user", &article("Alpha", "one", "dev", &["one"]))
        .expect("a");
    let second = store
        .create_article("local-user", &article("Beta", "two", "news", &["two"]))
        .expect("b");
    let third = store
        .create_article("local-user", &article("Gamma", "three", "dev", &["three"]))
        .expect("c");

    store
        .update_article(
            "local-user",
            first.id,
            &WikiArticlePatch {
                is_pinned: Some(true),
                ..WikiArticlePatch::default()
            },
        )
        .expect("pin");

    let listed = store
        .list_articles("local-user", &WikiFilter::default())
        .expect("list");
    assert_eq!(
        listed.iter().map(|row| row.id).collect::<Vec<_>>(),
        vec![first.id, third.id, second.id],
        "pinned first, then updated_at DESC / id DESC"
    );

    let pinned = store
        .list_articles(
            "local-user",
            &WikiFilter {
                pinned: Some(true),
                ..WikiFilter::default()
            },
        )
        .expect("pinned");
    assert_eq!(pinned.len(), 1);
    assert_eq!(pinned[0].id, first.id);

    let by_category = store
        .list_articles(
            "local-user",
            &WikiFilter {
                category: Some("dev".to_string()),
                ..WikiFilter::default()
            },
        )
        .expect("by category");
    assert_eq!(by_category.len(), 2);

    store
        .update_article(
            "local-user",
            second.id,
            &WikiArticlePatch {
                is_archived: Some(true),
                ..WikiArticlePatch::default()
            },
        )
        .expect("archive");

    let active = store
        .list_articles(
            "local-user",
            &WikiFilter {
                archived: Some(false),
                ..WikiFilter::default()
            },
        )
        .expect("active");
    assert_eq!(active.len(), 2);
    let archived = store
        .list_articles(
            "local-user",
            &WikiFilter {
                archived: Some(true),
                ..WikiFilter::default()
            },
        )
        .expect("archived");
    assert_eq!(archived.len(), 1);
    assert_eq!(archived[0].id, second.id);
}

#[test]
fn search_categories_and_stats_aggregate() {
    let (_directory, store) = adopted_store();
    let rust = store
        .create_article(
            "local-user",
            &article(
                "Rust Ownership",
                "memory safety",
                "dev",
                &["rust", "memory"],
            ),
        )
        .expect("rust");
    store
        .create_article(
            "local-user",
            &article("Campaign Plan", "launch plan", "marketing", &["plan"]),
        )
        .expect("marketing");
    let archived = store
        .create_article(
            "local-user",
            &article("Old Note", "deprecated", "dev", &["archive"]),
        )
        .expect("archived");
    store
        .update_article(
            "local-user",
            archived.id,
            &WikiArticlePatch {
                is_archived: Some(true),
                ..WikiArticlePatch::default()
            },
        )
        .expect("archive");

    let by_content = store
        .list_articles(
            "local-user",
            &WikiFilter {
                search: Some("memory".to_string()),
                archived: Some(false),
                ..WikiFilter::default()
            },
        )
        .expect("search content");
    assert_eq!(by_content.len(), 1);
    assert_eq!(by_content[0].id, rust.id);

    let by_category = store
        .list_articles(
            "local-user",
            &WikiFilter {
                search: Some("marketing".to_string()),
                ..WikiFilter::default()
            },
        )
        .expect("search category");
    assert_eq!(by_category.len(), 1);

    let by_tag = store
        .list_articles(
            "local-user",
            &WikiFilter {
                search: Some("plan".to_string()),
                ..WikiFilter::default()
            },
        )
        .expect("search tag");
    assert_eq!(by_tag.len(), 1);

    let categories = store
        .wiki_categories("local-user", true)
        .expect("categories");
    assert_eq!(
        categories,
        vec![("dev".to_string(), 2), ("marketing".to_string(), 1)]
    );
    let active_categories = store
        .wiki_categories("local-user", false)
        .expect("active categories");
    assert_eq!(
        active_categories,
        vec![("dev".to_string(), 1), ("marketing".to_string(), 1)]
    );

    let stats = store.wiki_stats("local-user").expect("stats");
    assert_eq!(stats.total, 3);
    assert_eq!(stats.pinned, 0);
    assert_eq!(stats.archived, 1);
    assert_eq!(
        stats.by_category,
        vec![("dev".to_string(), 2), ("marketing".to_string(), 1)]
    );
}

#[test]
fn wiki_is_actor_scoped() {
    let (_directory, store) = adopted_store();
    let article = store
        .create_article("local-user", &article("Private", "secret", "dev", &[]))
        .expect("create");

    store.ensure_actor("other-actor").expect("register");

    assert!(
        store
            .list_articles("other-actor", &WikiFilter::default())
            .expect("list")
            .is_empty()
    );
    assert!(matches!(
        store.get_article("other-actor", article.id),
        Err(StoreError::NotFound("wiki article"))
    ));
    assert!(matches!(
        store.update_article(
            "other-actor",
            article.id,
            &WikiArticlePatch {
                title: Some("Hijack".to_string()),
                ..WikiArticlePatch::default()
            },
        ),
        Err(StoreError::NotFound("wiki article"))
    ));
    assert!(matches!(
        store.delete_article("other-actor", article.id),
        Err(StoreError::NotFound("wiki article"))
    ));
    assert_eq!(store.wiki_stats("other-actor").expect("stats").total, 0);
}
