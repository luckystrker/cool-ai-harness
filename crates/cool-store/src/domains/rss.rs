//! RSS subscriptions and entries (legacy `rss_subscriptions` / `rss_entries`).
//!
//! Mirrors `backend/app/rss/service.py`. Entries are deduplicated in code by
//! `(subscription_id, guid)` because the legacy schema has no unique index on
//! that pair; `insert_entry_if_new` performs the same check the Python fetch
//! loop does before inserting.

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};

use crate::domains::common::{bounded_limit, collect_rows, query_one, user_id_for};
use crate::error::StoreError;
use crate::time::now_python;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RssSubscription {
    pub id: i64,
    pub user_id: i64,
    pub url: String,
    pub title: Option<String>,
    pub site_url: Option<String>,
    pub category: Option<String>,
    pub fetch_interval_minutes: i64,
    pub enabled: bool,
    pub last_fetched_at: Option<String>,
    pub last_error: Option<String>,
    pub entry_count: i64,
    pub created_at: String,
    pub updated_at: String,
}

impl RssSubscription {
    fn from_row(row: &Row<'_>) -> Result<Self, StoreError> {
        Ok(Self {
            id: row.get("id")?,
            user_id: row.get("user_id")?,
            url: row.get("url")?,
            title: row.get("title")?,
            site_url: row.get("site_url")?,
            category: row.get("category")?,
            fetch_interval_minutes: row.get("fetch_interval_minutes")?,
            enabled: row.get::<_, i64>("enabled")? != 0,
            last_fetched_at: row.get("last_fetched_at")?,
            last_error: row.get("last_error")?,
            entry_count: row.get("entry_count")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewRssSubscription {
    pub url: String,
    pub title: Option<String>,
    pub site_url: Option<String>,
    pub category: Option<String>,
    /// Defaults to 60, clamped to a minimum of 5 (Python `max(5, ...)`).
    pub fetch_interval_minutes: Option<i64>,
    pub enabled: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RssEntry {
    pub id: i64,
    pub subscription_id: i64,
    pub guid: String,
    pub title: Option<String>,
    pub link: Option<String>,
    pub author: Option<String>,
    pub summary: Option<String>,
    pub published_at: Option<String>,
    pub content_hash: Option<String>,
    pub is_read: bool,
    pub fetched_at: String,
    pub created_at: String,
    pub updated_at: String,
}

impl RssEntry {
    fn from_row(row: &Row<'_>) -> Result<Self, StoreError> {
        Ok(Self {
            id: row.get("id")?,
            subscription_id: row.get("subscription_id")?,
            guid: row.get("guid")?,
            title: row.get("title")?,
            link: row.get("link")?,
            author: row.get("author")?,
            summary: row.get("summary")?,
            published_at: row.get("published_at")?,
            content_hash: row.get("content_hash")?,
            is_read: row.get::<_, i64>("is_read")? != 0,
            fetched_at: row.get("fetched_at")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewRssEntry {
    pub guid: String,
    pub title: Option<String>,
    pub link: Option<String>,
    pub author: Option<String>,
    pub summary: Option<String>,
    pub published_at: Option<String>,
    pub content_hash: Option<String>,
}

/// Fetch a subscription while validating actor ownership.
fn fetch_subscription(
    connection: &Connection,
    actor_id: &str,
    subscription_id: i64,
) -> Result<RssSubscription, StoreError> {
    let user_id = user_id_for(connection, actor_id)?;
    query_one(
        connection,
        "SELECT * FROM rss_subscriptions WHERE id = ?1 AND user_id = ?2",
        params![subscription_id, user_id],
        RssSubscription::from_row,
    )?
    .ok_or(StoreError::NotFound("rss subscription"))
}

/// Fetch an entry whose parent subscription belongs to the actor.
fn fetch_entry(
    connection: &Connection,
    actor_id: &str,
    entry_id: i64,
) -> Result<RssEntry, StoreError> {
    let user_id = user_id_for(connection, actor_id)?;
    query_one(
        connection,
        "SELECT e.* FROM rss_entries e JOIN rss_subscriptions s ON s.id = e.subscription_id \
         WHERE e.id = ?1 AND s.user_id = ?2",
        params![entry_id, user_id],
        RssEntry::from_row,
    )?
    .ok_or(StoreError::NotFound("rss entry"))
}

impl crate::LegacyStore {
    /// Subscriptions for the actor, newest first (Python orders by `id DESC`).
    pub fn list_subscriptions(
        &self,
        actor_id: &str,
        category: Option<&str>,
        enabled: Option<bool>,
    ) -> Result<Vec<RssSubscription>, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        let mut statement = connection.prepare(
            "SELECT * FROM rss_subscriptions WHERE user_id = ?1 \
             AND (?2 IS NULL OR category = ?2) \
             AND (?3 IS NULL OR enabled = ?3) \
             ORDER BY id DESC",
        )?;
        let rows = statement.query(params![user_id, category, enabled.map(i64::from)])?;
        collect_rows(rows, RssSubscription::from_row)
    }

    pub fn get_subscription(
        &self,
        actor_id: &str,
        subscription_id: i64,
    ) -> Result<RssSubscription, StoreError> {
        let connection = self.connection()?;
        fetch_subscription(&connection, actor_id, subscription_id)
    }

    /// Create a subscription. Rejects a duplicate `(user_id, url)` like the
    /// Python service, which raises `ValueError` on an existing URL.
    pub fn create_subscription(
        &self,
        actor_id: &str,
        new: &NewRssSubscription,
    ) -> Result<RssSubscription, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        let duplicate: Option<i64> = connection
            .query_row(
                "SELECT id FROM rss_subscriptions WHERE user_id = ?1 AND url = ?2 LIMIT 1",
                params![user_id, new.url],
                |row| row.get(0),
            )
            .optional()?;
        if duplicate.is_some() {
            return Err(StoreError::Conflict(format!(
                "already subscribed to {}",
                new.url
            )));
        }
        let interval = new.fetch_interval_minutes.unwrap_or(60).max(5);
        let enabled = new.enabled.unwrap_or(true);
        let timestamp = now_python();
        connection.execute(
            "INSERT INTO rss_subscriptions(created_at, updated_at, user_id, url, title, site_url,
               category, fetch_interval_minutes, enabled, entry_count)
             VALUES (?1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0)",
            params![
                timestamp,
                user_id,
                new.url,
                new.title,
                new.site_url,
                new.category,
                interval,
                i64::from(enabled),
            ],
        )?;
        let id = connection.last_insert_rowid();
        drop(connection);
        self.get_subscription(actor_id, id)
    }

    /// Delete a subscription and all of its entries (Python `unsubscribe`).
    pub fn delete_subscription(
        &self,
        actor_id: &str,
        subscription_id: i64,
    ) -> Result<(), StoreError> {
        let mut connection = self.connection()?;
        fetch_subscription(&connection, actor_id, subscription_id)?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "DELETE FROM rss_entries WHERE subscription_id = ?1",
            [subscription_id],
        )?;
        transaction.execute(
            "DELETE FROM rss_subscriptions WHERE id = ?1",
            [subscription_id],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Record the outcome of a fetch: timestamps, error and stored entry count.
    ///
    /// `entry_count` is the absolute number of entries now stored for the
    /// subscription (Python keeps the same counter on `RssSubscription`).
    pub fn record_fetch_result(
        &self,
        actor_id: &str,
        subscription_id: i64,
        fetched_at: &str,
        error: Option<&str>,
        entry_count: i64,
    ) -> Result<(), StoreError> {
        let connection = self.connection()?;
        fetch_subscription(&connection, actor_id, subscription_id)?;
        connection.execute(
            "UPDATE rss_subscriptions SET last_fetched_at = ?1, last_error = ?2, entry_count = ?3,
               updated_at = ?4 WHERE id = ?5",
            params![
                fetched_at,
                error,
                entry_count,
                now_python(),
                subscription_id
            ],
        )?;
        Ok(())
    }

    /// Insert an entry unless `(subscription_id, guid)` already exists.
    ///
    /// Returns `None` for a duplicate GUID (Python dedups in code because the
    /// legacy schema has no unique index on the pair). A successful insert
    /// bumps the subscription's `entry_count` and stamps `fetched_at = now`.
    pub fn insert_entry_if_new(
        &self,
        actor_id: &str,
        subscription_id: i64,
        new: &NewRssEntry,
    ) -> Result<Option<RssEntry>, StoreError> {
        let mut connection = self.connection()?;
        fetch_subscription(&connection, actor_id, subscription_id)?;
        let existing: Option<i64> = connection
            .query_row(
                "SELECT id FROM rss_entries WHERE subscription_id = ?1 AND guid = ?2 LIMIT 1",
                params![subscription_id, new.guid],
                |row| row.get(0),
            )
            .optional()?;
        if existing.is_some() {
            return Ok(None);
        }
        let timestamp = now_python();
        let transaction = connection.transaction()?;
        transaction.execute(
            "INSERT INTO rss_entries(created_at, updated_at, subscription_id, guid, title, link,
               author, summary, published_at, content_hash, is_read, fetched_at)
             VALUES (?1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0, ?1)",
            params![
                timestamp,
                subscription_id,
                new.guid,
                new.title,
                new.link,
                new.author,
                new.summary,
                new.published_at,
                new.content_hash,
            ],
        )?;
        let id = transaction.last_insert_rowid();
        transaction.execute(
            "UPDATE rss_subscriptions SET entry_count = entry_count + 1, updated_at = ?1 \
             WHERE id = ?2",
            params![timestamp, subscription_id],
        )?;
        transaction.commit()?;
        Ok(Some(RssEntry {
            id,
            subscription_id,
            guid: new.guid.clone(),
            title: new.title.clone(),
            link: new.link.clone(),
            author: new.author.clone(),
            summary: new.summary.clone(),
            published_at: new.published_at.clone(),
            content_hash: new.content_hash.clone(),
            is_read: false,
            fetched_at: timestamp.clone(),
            created_at: timestamp.clone(),
            updated_at: timestamp,
        }))
    }

    /// Entries for one subscription, newest first.
    ///
    /// Matches Python `published_at DESC NULLS LAST` with a stable `id DESC`
    /// tiebreak.
    pub fn list_entries(
        &self,
        actor_id: &str,
        subscription_id: i64,
        limit: Option<usize>,
        unread_only: bool,
    ) -> Result<Vec<RssEntry>, StoreError> {
        let connection = self.connection()?;
        fetch_subscription(&connection, actor_id, subscription_id)?;
        let mut statement = connection.prepare(
            "SELECT * FROM rss_entries WHERE subscription_id = ?1 \
             AND (?2 = 0 OR is_read = 0) \
             ORDER BY (published_at IS NULL) ASC, published_at DESC, id DESC LIMIT ?3",
        )?;
        let rows = statement.query(params![
            subscription_id,
            i64::from(unread_only),
            bounded_limit(limit, 50, 200),
        ])?;
        collect_rows(rows, RssEntry::from_row)
    }

    /// All entries across the actor's subscriptions, newest first.
    pub fn list_all_entries(
        &self,
        actor_id: &str,
        limit: Option<usize>,
        unread_only: bool,
    ) -> Result<Vec<RssEntry>, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        let mut statement = connection.prepare(
            "SELECT e.* FROM rss_entries e JOIN rss_subscriptions s ON s.id = e.subscription_id \
             WHERE s.user_id = ?1 AND (?2 = 0 OR e.is_read = 0) \
             ORDER BY (e.published_at IS NULL) ASC, e.published_at DESC, e.id DESC LIMIT ?3",
        )?;
        let rows = statement.query(params![
            user_id,
            i64::from(unread_only),
            bounded_limit(limit, 50, 200),
        ])?;
        collect_rows(rows, RssEntry::from_row)
    }

    /// Flip an entry's read state, validated through its subscription owner.
    pub fn mark_entry_read(
        &self,
        actor_id: &str,
        entry_id: i64,
        read: bool,
    ) -> Result<RssEntry, StoreError> {
        let connection = self.connection()?;
        fetch_entry(&connection, actor_id, entry_id)?;
        connection.execute(
            "UPDATE rss_entries SET is_read = ?1, updated_at = ?2 WHERE id = ?3",
            params![i64::from(read), now_python(), entry_id],
        )?;
        fetch_entry(&connection, actor_id, entry_id)
    }
}

/// Read a subscription's stored entry count (diagnostics/tests).
#[allow(dead_code)]
pub fn subscription_entry_count(
    connection: &Connection,
    subscription_id: i64,
) -> Result<Option<i64>, StoreError> {
    Ok(connection
        .query_row(
            "SELECT entry_count FROM rss_subscriptions WHERE id = ?1",
            [subscription_id],
            |row| row.get(0),
        )
        .optional()?)
}
