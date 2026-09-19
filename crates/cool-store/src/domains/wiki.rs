//! Wiki / knowledge-base articles (legacy `wiki_articles`).
//!
//! Mirrors `backend/app/wiki/__init__.py`. Search is SQL `LIKE` over
//! title/content/category/tags (Python does not use FTS5 for the wiki), and
//! listing is pinned-first then `updated_at DESC`. Articles are actor-scoped by
//! `user_id`; new articles record the resolved actor as their owner.

use rusqlite::{Connection, Row, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::domains::common::{
    bounded_limit, collect_rows, json_text, parse_json, query_one, user_id_for,
};
use crate::error::StoreError;
use crate::time::now_python;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WikiArticle {
    pub id: i64,
    pub title: String,
    pub content: String,
    pub category: String,
    pub tags: Option<Value>,
    pub source: String,
    pub source_memory_id: Option<i64>,
    pub user_id: Option<i64>,
    pub project_key: Option<String>,
    pub is_pinned: bool,
    pub is_archived: bool,
    pub version: i64,
    pub metadata: Option<Value>,
    pub created_at: String,
    pub updated_at: String,
}

impl WikiArticle {
    fn from_row(row: &Row<'_>) -> Result<Self, StoreError> {
        Ok(Self {
            id: row.get("id")?,
            title: row.get("title")?,
            content: row.get("content")?,
            category: row.get("category")?,
            tags: parse_json(row.get("tags")?)?,
            source: row.get("source")?,
            source_memory_id: row.get("source_memory_id")?,
            user_id: row.get("user_id")?,
            project_key: row.get("project_key")?,
            is_pinned: row.get::<_, i64>("is_pinned")? != 0,
            is_archived: row.get::<_, i64>("is_archived")? != 0,
            version: row.get("version")?,
            metadata: parse_json(row.get("metadata_")?)?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewWikiArticle {
    pub title: String,
    pub content: String,
    pub category: String,
    /// Stored as `[]` when absent, matching Python's `tags or []`.
    pub tags: Option<Value>,
    pub source: String,
    pub source_memory_id: Option<i64>,
    pub project_key: Option<String>,
    pub metadata: Option<Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WikiArticlePatch {
    pub title: Option<String>,
    pub content: Option<String>,
    pub category: Option<String>,
    pub tags: Option<Value>,
    pub is_pinned: Option<bool>,
    pub is_archived: Option<bool>,
}

#[derive(Clone, Debug, Default)]
pub struct WikiFilter {
    /// `Some(true)` only archived, `Some(false)` only active, `None` both.
    pub archived: Option<bool>,
    pub category: Option<String>,
    pub project_key: Option<String>,
    /// SQL `LIKE` across title/content/category/tags (substring match).
    pub search: Option<String>,
    /// Exact tag membership inside the JSON `tags` array.
    pub tag: Option<String>,
    pub pinned: Option<bool>,
    pub limit: Option<usize>,
    pub offset: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WikiStats {
    pub total: i64,
    pub pinned: i64,
    pub archived: i64,
    pub by_category: Vec<(String, i64)>,
}

/// Fetch an article while validating actor ownership.
fn fetch_article(
    connection: &Connection,
    actor_id: &str,
    article_id: i64,
) -> Result<WikiArticle, StoreError> {
    let user_id = user_id_for(connection, actor_id)?;
    query_one(
        connection,
        "SELECT * FROM wiki_articles WHERE id = ?1 AND user_id = ?2",
        params![article_id, user_id],
        WikiArticle::from_row,
    )?
    .ok_or(StoreError::NotFound("wiki article"))
}

impl crate::LegacyStore {
    /// List the actor's articles: pinned first, then `updated_at DESC`.
    pub fn list_articles(
        &self,
        actor_id: &str,
        filter: &WikiFilter,
    ) -> Result<Vec<WikiArticle>, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        let mut statement = connection.prepare(
            "SELECT * FROM wiki_articles WHERE user_id = ?1 \
             AND (?2 IS NULL OR is_archived = ?2) \
             AND (?3 IS NULL OR category = ?3) \
             AND (?4 IS NULL OR project_key = ?4) \
             AND (?5 IS NULL OR is_pinned = ?5) \
             AND (?6 IS NULL OR title LIKE ?6 OR content LIKE ?6 OR category LIKE ?6 \
                  OR tags LIKE ?6) \
             AND (?7 IS NULL OR tags LIKE ?7) \
             ORDER BY is_pinned DESC, updated_at DESC, id DESC LIMIT ?8 OFFSET ?9",
        )?;
        let rows = statement.query(params![
            user_id,
            filter.archived.map(i64::from),
            filter.category,
            filter.project_key,
            filter.pinned.map(i64::from),
            filter.search.as_ref().map(|value| format!("%{value}%")),
            filter.tag.as_ref().map(|value| format!("%\"{value}\"%")),
            bounded_limit(filter.limit, 100, 500),
            filter.offset as i64,
        ])?;
        collect_rows(rows, WikiArticle::from_row)
    }

    pub fn get_article(&self, actor_id: &str, article_id: i64) -> Result<WikiArticle, StoreError> {
        let connection = self.connection()?;
        fetch_article(&connection, actor_id, article_id)
    }

    pub fn create_article(
        &self,
        actor_id: &str,
        new: &NewWikiArticle,
    ) -> Result<WikiArticle, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        let timestamp = now_python();
        let tags = new.tags.clone().unwrap_or_else(|| json!([]));
        connection.execute(
            "INSERT INTO wiki_articles(created_at, updated_at, title, content, category, tags,
               source, source_memory_id, user_id, project_key, is_pinned, is_archived, version,
               metadata_)
             VALUES (?1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0, 0, 1, ?10)",
            params![
                timestamp,
                new.title,
                new.content,
                new.category,
                json_text(&Some(tags))?,
                new.source,
                new.source_memory_id,
                user_id,
                new.project_key,
                json_text(&new.metadata)?,
            ],
        )?;
        let id = connection.last_insert_rowid();
        drop(connection);
        self.get_article(actor_id, id)
    }

    /// Patch an article. `updated_at` always advances (Python commits on every
    /// call) and `version` increments whenever `content` is supplied, even when
    /// the value is unchanged (matching Python's `version += 1`).
    pub fn update_article(
        &self,
        actor_id: &str,
        article_id: i64,
        patch: &WikiArticlePatch,
    ) -> Result<WikiArticle, StoreError> {
        let connection = self.connection()?;
        fetch_article(&connection, actor_id, article_id)?;
        let mut assignments: Vec<&str> = Vec::new();
        let mut values: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(title) = &patch.title {
            assignments.push("title = ?");
            values.push(Box::new(title.clone()));
        }
        if let Some(content) = &patch.content {
            assignments.push("content = ?");
            values.push(Box::new(content.clone()));
            assignments.push("version = version + 1");
        }
        if let Some(category) = &patch.category {
            assignments.push("category = ?");
            values.push(Box::new(category.clone()));
        }
        if patch.tags.is_some() {
            assignments.push("tags = ?");
            values.push(Box::new(json_text(&patch.tags)?));
        }
        if let Some(pinned) = patch.is_pinned {
            assignments.push("is_pinned = ?");
            values.push(Box::new(i64::from(pinned)));
        }
        if let Some(archived) = patch.is_archived {
            assignments.push("is_archived = ?");
            values.push(Box::new(i64::from(archived)));
        }
        assignments.push("updated_at = ?");
        values.push(Box::new(now_python()));
        values.push(Box::new(article_id));
        let sql = format!(
            "UPDATE wiki_articles SET {} WHERE id = ?",
            assignments.join(", ")
        );
        let references: Vec<&dyn rusqlite::ToSql> =
            values.iter().map(|value| value.as_ref()).collect();
        connection.execute(&sql, references.as_slice())?;
        drop(connection);
        self.get_article(actor_id, article_id)
    }

    pub fn delete_article(&self, actor_id: &str, article_id: i64) -> Result<(), StoreError> {
        let connection = self.connection()?;
        fetch_article(&connection, actor_id, article_id)?;
        connection.execute("DELETE FROM wiki_articles WHERE id = ?1", [article_id])?;
        Ok(())
    }

    /// Distinct categories with article counts, ordered by name.
    pub fn wiki_categories(
        &self,
        actor_id: &str,
        include_archived: bool,
    ) -> Result<Vec<(String, i64)>, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        let mut statement = connection.prepare(
            "SELECT category, COUNT(*) FROM wiki_articles WHERE user_id = ?1 \
             AND (?2 = 1 OR is_archived = 0) GROUP BY category ORDER BY category",
        )?;
        let rows = statement.query(params![user_id, i64::from(include_archived)])?;
        collect_rows(rows, |row| Ok((row.get(0)?, row.get(1)?)))
    }

    /// Aggregate counts for the actor's wiki articles.
    pub fn wiki_stats(&self, actor_id: &str) -> Result<WikiStats, StoreError> {
        let connection = self.connection()?;
        let user_id = user_id_for(&connection, actor_id)?;
        let total: i64 = connection.query_row(
            "SELECT COUNT(*) FROM wiki_articles WHERE user_id = ?1",
            [user_id],
            |row| row.get(0),
        )?;
        let pinned: i64 = connection.query_row(
            "SELECT COUNT(*) FROM wiki_articles WHERE user_id = ?1 AND is_pinned = 1",
            [user_id],
            |row| row.get(0),
        )?;
        let archived: i64 = connection.query_row(
            "SELECT COUNT(*) FROM wiki_articles WHERE user_id = ?1 AND is_archived = 1",
            [user_id],
            |row| row.get(0),
        )?;
        let mut statement = connection.prepare(
            "SELECT category, COUNT(*) FROM wiki_articles WHERE user_id = ?1 \
             GROUP BY category ORDER BY category",
        )?;
        let by_category = collect_rows(statement.query([user_id])?, |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;
        Ok(WikiStats {
            total,
            pinned,
            archived,
            by_category,
        })
    }
}
