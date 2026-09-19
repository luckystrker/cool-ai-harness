//! Memory retrieval, ranking and lifecycle parity with the Python runtime (M10).
//!
//! This module replicates `backend/app/memory/retrieval.py` and the pure-SQL
//! parts of `backend/app/memory/lifecycle.py` against the legacy schema.
//!
//! # Retrieval formula
//!
//! `search` reproduces Python's pipeline exactly:
//!
//! 1. **Preferences.** Active `preference` memories are always candidates
//!    (`ORDER BY importance DESC LIMIT 10`) and carry no FTS relevance signal.
//! 2. **FTS5.** The query is tokenized with Python's `_fts_terms`
//!    (`re.sub(r"[^\w]", "", word)`, keep tokens of length > 1) and joined with
//!    ` OR ` after each token is quoted as an FTS5 phrase (`"term"`, embedded
//!    quotes doubled). Candidate rows come from
//!    `SELECT rowid, rank FROM memory_fts WHERE memory_fts MATCH ? AND rank < 0
//!      ORDER BY rank LIMIT limit*3`
//!    (BM25: lower/more negative is better). Per-hit relevance is
//!    `1 / (1 + |rank| / 3)`.
//! 3. **Vector leg.** Python runs it only when `query_embedding` is supplied;
//!    [`MemoryQuery`] carries no embedding, so the requested hybrid mode
//!    degrades to FTS-only, exactly like Python with `query_embedding=None`.
//! 4. **Entity leg.** Query words longer than 3 characters (max 5) are matched
//!    case-insensitively against entity names/aliases (max 6 entities) and
//!    their linked active memories (max 3, relevance default 0.35).
//! 5. **Fallback.** While fewer than `limit` candidates exist, recent important
//!    memories are appended (scope-visible, `limit*2`, importance/updated desc).
//! 6. **Rerank.** Stable sort by the composite score, descending:
//!
//!    ```text
//!    relevance present: 0.40*rel + 0.20*importance + 0.15*recency
//!                       + 0.10*confidence + 0.15*type_priority
//!    relevance absent:  0.25*importance + 0.25*recency
//!                       + 0.15*confidence + 0.35*type_priority
//!    recency = 1 / (1 + age_days / 30)       age_days = (now - updated_at) / 86400
//!    type_priority = preference 1.0 | procedural 0.8 | semantic 0.6
//!                    | episodic 0.4 | other 0.5
//!    ```
//!
//!    Python uses a stable `list.sort`, so ties keep insertion order
//!    (preferences → FTS rank → entity → fallback); Rust uses the stable
//!    `slice::sort_by` with the same insertion order.
//!
//! # Documented divergences from Python
//!
//! - Python accepts a `scope` argument on `retrieve_memories` but never reads
//!   it. [`MemoryQuery::scope`] is kept for API parity and is likewise ignored.
//! - Python always restricts candidates to `status == "active"`; Rust adds the
//!   Rust-only `status`/`include_archived` knobs (defaults reproduce Python).
//! - Python's `_touch_access` increments `access_count`/`last_accessed_at` on
//!   every recall. Rust's `search` is read-only so repeated calls are
//!   idempotent; returned rows/scores are unaffected.
//! - `MemoryQuery` has no `agent_id`, so agent-scoped memories are invisible
//!   (matching Python when `agent_id=None`).

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use regex::Regex;
use rusqlite::{Connection, ToSql, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::LegacyStore;
use crate::domains::common::{collect_rows, query_one, require_conversation, user_id_for};
use crate::domains::memory::{
    Entity, MEMORY_STATUS_ACTIVE, MEMORY_TYPE_PREFERENCE, MemoryItem, SCOPE_AGENT,
    SCOPE_CONVERSATION, SCOPE_GLOBAL,
};
use crate::error::StoreError;
use crate::time::{parse_python_datetime, python_datetime};

/// Retrieval request mirroring Python `recall(...)`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryQuery {
    pub text: Option<String>,
    pub memory_type: Option<String>,
    /// Accepted for API parity; Python's `retrieve_memories` ignores it.
    pub scope: Option<String>,
    /// Rust-only status override; `None` means Python's `active`.
    pub status: Option<String>,
    pub conversation_id: Option<i64>,
    pub limit: usize,
    /// Rust-only extension: include non-active statuses. Default is Python-exact.
    pub include_archived: bool,
    /// Requested hybrid (FTS + vector) mode. Without an embedding the vector
    /// leg is skipped and this degrades to FTS-only (Python parity).
    pub hybrid: bool,
}

impl Default for MemoryQuery {
    fn default() -> Self {
        Self {
            text: None,
            memory_type: None,
            scope: None,
            status: None,
            conversation_id: None,
            limit: 10,
            include_archived: false,
            hybrid: false,
        }
    }
}

/// A ranked memory. `score` is the composite rerank score; `fts_rank` is the
/// raw BM25 rank (lower is better) when the FTS leg matched the item.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryHit {
    pub item: MemoryItem,
    pub score: f64,
    pub fts_rank: Option<f64>,
    pub vector_score: Option<f64>,
}

// --- Composite scoring constants (retrieval.py) ---

const W_IMPORTANCE: f64 = 0.25;
const W_RECENCY: f64 = 0.25;
const W_CONFIDENCE: f64 = 0.15;
const W_TYPE: f64 = 0.35;

const W_REL: f64 = 0.40;
const W_REL_IMPORTANCE: f64 = 0.20;
const W_REL_RECENCY: f64 = 0.15;
const W_REL_CONFIDENCE: f64 = 0.10;
const W_REL_TYPE: f64 = 0.15;

/// `Settings.memory_fts_min_rank` default (0.0 = disabled).
const MIN_FTS_RANK: f64 = 0.0;
const PREFERENCE_LIMIT: usize = 10;
const ENTITY_RELEVANCE: f64 = 0.35;
const ENTITY_MEMORY_LIMIT: usize = 3;
const ENTITY_MATCH_LIMIT: usize = 6;

fn type_priority(memory_type: &str) -> f64 {
    match memory_type {
        "preference" => 1.0,
        "procedural" => 0.8,
        "semantic" => 0.6,
        "episodic" => 0.4,
        _ => 0.5,
    }
}

/// Current UTC wall clock as microseconds since the Unix epoch.
fn current_micros() -> i64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    now.as_secs() as i64 * 1_000_000 + now.subsec_micros() as i64
}

/// Parse a Python/SQLAlchemy or RFC 3339 timestamp to microseconds.
pub(crate) fn timestamp_micros(value: &str) -> Option<i64> {
    let seconds = parse_python_datetime(value)?;
    let normalized = value.trim().replace('T', " ");
    let normalized = normalized.strip_suffix('Z').unwrap_or(&normalized);
    let fraction = normalized
        .split_once('.')
        .map(|(_, fraction)| fraction)
        .unwrap_or("");
    let mut micros: u32 = 0;
    let mut digits = 0;
    for character in fraction.chars() {
        if !character.is_ascii_digit() || digits >= 6 {
            break;
        }
        micros = micros * 10 + character.to_digit(10).unwrap_or(0);
        digits += 1;
    }
    let micros = micros * 10u32.pow(6 - digits);
    Some(seconds * 1_000_000 + micros as i64)
}

fn score_memory(item: &MemoryItem, now_micros: i64, relevance: Option<f64>) -> f64 {
    score_breakdown(item, now_micros, relevance).total
}

/// Component breakdown of the composite memory score, mirroring
/// `retrieval.score_memory` for the explainability surface.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryScoreBreakdown {
    pub importance: f64,
    pub recency: f64,
    pub confidence: f64,
    pub type_priority: f64,
    pub age_days: f64,
    pub total: f64,
}

/// "Why is this remembered" projection, mirroring `memory.service.explain_memory`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryExplanation {
    pub memory_id: i64,
    pub source: String,
    pub scope: String,
    pub status: String,
    pub pinned: bool,
    pub confidence: f64,
    pub importance: f64,
    pub memory_type: String,
    pub conversation_id: Option<i64>,
    pub agent_id: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
    pub last_accessed_at: Option<String>,
    pub access_count: i64,
    pub score: MemoryScoreBreakdown,
}

impl crate::LegacyStore {
    /// Explain why a memory is retained: provenance, lifecycle metadata and
    /// the composite score breakdown (actor-scoped).
    pub fn explain_memory(
        &self,
        actor_id: &str,
        memory_id: i64,
    ) -> Result<MemoryExplanation, StoreError> {
        let item = self.get_memory_item(actor_id, memory_id)?;
        Ok(MemoryExplanation {
            memory_id: item.id,
            source: item.source.clone(),
            scope: item.scope.clone(),
            status: item.status.clone(),
            pinned: item.pinned,
            confidence: item.confidence,
            importance: item.importance,
            memory_type: item.memory_type.clone(),
            conversation_id: item.conversation_id,
            agent_id: item.agent_id,
            created_at: item.created_at.clone(),
            updated_at: item.updated_at.clone(),
            last_accessed_at: item.last_accessed_at.clone(),
            access_count: item.access_count,
            score: score_breakdown(&item, current_micros(), None),
        })
    }
}

fn score_breakdown(
    item: &MemoryItem,
    now_micros: i64,
    relevance: Option<f64>,
) -> MemoryScoreBreakdown {
    let age_days = match timestamp_micros(&item.updated_at) {
        Some(updated) => (now_micros - updated) as f64 / 1_000_000.0 / 86_400.0,
        None => 30.0,
    };
    let recency = 1.0 / (1.0 + age_days / 30.0);
    let priority = type_priority(&item.memory_type);
    let (importance, recency_weight, confidence, type_weight, relevance_weight) = match relevance {
        Some(_) => (
            W_REL_IMPORTANCE,
            W_REL_RECENCY,
            W_REL_CONFIDENCE,
            W_REL_TYPE,
            W_REL,
        ),
        None => (W_IMPORTANCE, W_RECENCY, W_CONFIDENCE, W_TYPE, 0.0),
    };
    let total = relevance_weight * relevance.unwrap_or(0.0)
        + importance * item.importance
        + recency_weight * recency
        + confidence * item.confidence
        + type_weight * priority;
    MemoryScoreBreakdown {
        importance: importance * item.importance,
        recency: recency_weight * recency,
        confidence: confidence * item.confidence,
        type_priority: type_weight * priority,
        age_days,
        total,
    }
}

// --- FTS5 query helpers (retrieval.py) ---

fn fts_non_word() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r"[^\w]").expect("valid FTS term regex"))
}

fn fts_terms(query: &str) -> Vec<String> {
    let regex = fts_non_word();
    query
        .split_whitespace()
        .map(|word| regex.replace_all(word, "").to_string())
        .filter(|word| word.chars().count() > 1)
        .collect()
}

fn fts_quote(term: &str) -> String {
    format!("\"{}\"", term.replace('"', "\"\""))
}

// --- Visibility / filters ---

fn is_visible(item: &MemoryItem, conversation_id: Option<i64>, project_key: Option<&str>) -> bool {
    match item.scope.as_str() {
        SCOPE_GLOBAL => true,
        // Agent-scoped memories need an active agent_id; MemoryQuery has none
        // (Python with agent_id=None behaves identically).
        SCOPE_AGENT => false,
        SCOPE_CONVERSATION => {
            if conversation_id.is_some() && item.conversation_id == conversation_id {
                return true;
            }
            if let Some(project_key) = project_key
                && let Some(Value::Object(map)) = &item.structured
                && map.get("_project_key").and_then(Value::as_str) == Some(project_key)
            {
                return true;
            }
            false
        }
        _ => false,
    }
}

fn matches_status(item: &MemoryItem, query: &MemoryQuery) -> bool {
    if query.include_archived {
        return true;
    }
    item.status == query.status.as_deref().unwrap_or(MEMORY_STATUS_ACTIVE)
}

fn matches_type(item: &MemoryItem, query: &MemoryQuery) -> bool {
    match query.memory_type.as_deref() {
        Some(memory_type) => item.memory_type == memory_type,
        None => true,
    }
}

fn conversation_workdir(
    connection: &Connection,
    conversation_id: i64,
) -> Result<Option<String>, StoreError> {
    Ok(query_one(
        connection,
        "SELECT working_directory FROM conversations WHERE id = ?1",
        [conversation_id],
        |row| Ok(row.get(0)?),
    )?
    .flatten())
}

fn fetch_item(
    connection: &Connection,
    user_id: i64,
    memory_id: i64,
) -> Result<Option<MemoryItem>, StoreError> {
    query_one(
        connection,
        "SELECT * FROM memory_items WHERE id = ?1 AND user_id = ?2",
        params![memory_id, user_id],
        MemoryItem::from_row,
    )
}

// --- Legs ---

fn fetch_preferences(connection: &Connection, user_id: i64) -> Result<Vec<MemoryItem>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT * FROM memory_items WHERE user_id = ?1 AND memory_type = ?2 AND status = ?3 \
         ORDER BY importance DESC LIMIT ?4",
    )?;
    let rows = statement.query(params![
        user_id,
        MEMORY_TYPE_PREFERENCE,
        MEMORY_STATUS_ACTIVE,
        PREFERENCE_LIMIT as i64,
    ])?;
    collect_rows(rows, MemoryItem::from_row)
}

/// FTS5 leg. Returns `(item, bm25_rank)` in ascending rank order.
fn fts5_search(
    connection: &Connection,
    query: &MemoryQuery,
    text: &str,
    user_id: i64,
    project_key: Option<&str>,
) -> Result<Vec<(MemoryItem, f64)>, StoreError> {
    if !crate::migrations::table_exists(connection, "memory_fts")? {
        return Ok(Vec::new());
    }
    let terms = fts_terms(text);
    if terms.is_empty() {
        return Ok(Vec::new());
    }
    let fts_query = terms
        .iter()
        .take(16)
        .map(|term| fts_quote(term))
        .collect::<Vec<_>>()
        .join(" OR ");
    let limit = query.limit.max(1).saturating_mul(3) as i64;
    let ranked: Vec<(i64, f64)> = {
        let mut statement = connection.prepare(
            "SELECT rowid, rank FROM memory_fts \
             WHERE memory_fts MATCH ?1 AND rank < ?2 ORDER BY rank LIMIT ?3",
        )?;
        let mut rows = statement.query(params![fts_query, MIN_FTS_RANK, limit])?;
        let mut collected = Vec::new();
        while let Some(row) = rows.next()? {
            collected.push((row.get::<_, i64>(0)?, row.get::<_, f64>(1)?));
        }
        collected
    };
    let mut out = Vec::new();
    for (memory_id, rank) in ranked {
        let Some(item) = fetch_item(connection, user_id, memory_id)? else {
            continue;
        };
        if !matches_status(&item, query)
            || !is_visible(&item, query.conversation_id, project_key)
            || !matches_type(&item, query)
        {
            continue;
        }
        out.push((item, rank));
    }
    Ok(out)
}

fn batch_match_entities(
    connection: &Connection,
    user_id: i64,
    words: &[String],
    limit: usize,
) -> Result<Vec<Entity>, StoreError> {
    if words.is_empty() {
        return Ok(Vec::new());
    }
    let fetch_limit = (limit * 10) as i64;
    let mut statement = connection
        .prepare("SELECT * FROM entities WHERE user_id = ?1 ORDER BY updated_at DESC LIMIT ?2")?;
    let rows = statement.query(params![user_id, fetch_limit])?;
    let entities = collect_rows(rows, Entity::from_row)?;
    let words_lower: Vec<String> = words.iter().map(|word| word.to_lowercase()).collect();
    let mut matched = Vec::new();
    for entity in entities {
        let name_lower = entity.name.to_lowercase();
        let aliases_lower: Vec<String> = match &entity.aliases {
            Some(Value::Array(aliases)) => aliases
                .iter()
                .filter_map(Value::as_str)
                .map(|alias| alias.to_lowercase())
                .collect(),
            _ => Vec::new(),
        };
        let hit = words_lower.iter().any(|word| {
            name_lower.contains(word) || aliases_lower.iter().any(|alias| alias.contains(word))
        });
        if hit {
            matched.push(entity);
            if matched.len() >= limit {
                break;
            }
        }
    }
    Ok(matched)
}

/// Entity-driven recall leg (max [`ENTITY_MEMORY_LIMIT`] memories).
fn entity_search(
    connection: &Connection,
    query: &MemoryQuery,
    text: &str,
    user_id: i64,
    project_key: Option<&str>,
) -> Result<Vec<MemoryItem>, StoreError> {
    let words: Vec<String> = text
        .split_whitespace()
        .map(|word| {
            word.trim_matches(|character: char| ".,;:!?()[]\"'".contains(character))
                .to_string()
        })
        .filter(|word| word.chars().count() > 3)
        .take(5)
        .collect();
    if words.is_empty() {
        return Ok(Vec::new());
    }
    let entities = batch_match_entities(connection, user_id, &words, ENTITY_MATCH_LIMIT)?;
    if entities.is_empty() {
        return Ok(Vec::new());
    }
    let mut seen: HashSet<i64> = HashSet::new();
    let mut out = Vec::new();
    for entity in entities {
        let mut statement = connection.prepare(
            "SELECT m.* FROM memory_items m \
             JOIN memory_item_entities l ON l.memory_id = m.id \
             WHERE l.entity_id = ?1 AND m.user_id = ?2 AND m.status = ?3 \
             ORDER BY m.updated_at DESC",
        )?;
        let rows = statement.query(params![entity.id, user_id, MEMORY_STATUS_ACTIVE])?;
        let memories = collect_rows(rows, MemoryItem::from_row)?;
        for memory in memories {
            if seen.contains(&memory.id)
                || !is_visible(&memory, query.conversation_id, project_key)
                || !matches_type(&memory, query)
            {
                continue;
            }
            seen.insert(memory.id);
            out.push(memory);
            if out.len() >= ENTITY_MEMORY_LIMIT {
                return Ok(out);
            }
        }
    }
    Ok(out)
}

/// Fallback leg: recent important memories visible to the context.
fn fetch_recent_important(
    connection: &Connection,
    query: &MemoryQuery,
    user_id: i64,
    project_key: Option<&str>,
    limit: usize,
) -> Result<Vec<MemoryItem>, StoreError> {
    let mut sql = String::from("SELECT * FROM memory_items WHERE user_id = ?1");
    let mut values: Vec<Box<dyn ToSql>> = vec![Box::new(user_id)];
    if !query.include_archived {
        sql.push_str(" AND status = ?");
        values.push(Box::new(
            query
                .status
                .clone()
                .unwrap_or_else(|| MEMORY_STATUS_ACTIVE.to_string()),
        ));
    }
    if let Some(memory_type) = &query.memory_type {
        sql.push_str(" AND memory_type = ?");
        values.push(Box::new(memory_type.clone()));
    }
    sql.push_str(" AND (scope = 'global'");
    if query.conversation_id.is_some() {
        sql.push_str(" OR (scope = 'conversation' AND conversation_id = ?)");
        values.push(Box::new(query.conversation_id));
    }
    if let Some(project_key) = project_key {
        sql.push_str(
            " OR (scope = 'conversation' AND json_extract(structured, '$._project_key') = ?)",
        );
        values.push(Box::new(project_key.to_string()));
    }
    sql.push_str(") ORDER BY importance DESC, updated_at DESC LIMIT ?");
    values.push(Box::new(limit as i64));
    let mut statement = connection.prepare(&sql)?;
    let references: Vec<&dyn ToSql> = values.iter().map(|value| value.as_ref()).collect();
    let rows = statement.query(references.as_slice())?;
    collect_rows(rows, MemoryItem::from_row)
}

// --- Public API ---

/// Search memories visible to the actor using the current wall clock.
pub fn search(
    store: &LegacyStore,
    actor_id: &str,
    query: &MemoryQuery,
) -> Result<Vec<MemoryHit>, StoreError> {
    search_at(store, actor_id, query, current_micros())
}

/// Search memories with an explicit `now` (microseconds since the Unix epoch).
/// Used by parity tests to freeze the recency term.
pub fn search_at(
    store: &LegacyStore,
    actor_id: &str,
    query: &MemoryQuery,
    now_micros: i64,
) -> Result<Vec<MemoryHit>, StoreError> {
    let connection = store.connection()?;
    let user_id = user_id_for(&connection, actor_id)?;
    if let Some(conversation_id) = query.conversation_id {
        require_conversation(&connection, actor_id, conversation_id)?;
    }
    let project_key = match query.conversation_id {
        Some(conversation_id) => conversation_workdir(&connection, conversation_id)?,
        None => None,
    };
    let limit = query.limit.max(1);

    let mut results: Vec<MemoryItem> = Vec::new();
    let mut seen: HashSet<i64> = HashSet::new();
    let mut relevance: HashMap<i64, f64> = HashMap::new();
    let mut fts_ranks: HashMap<i64, f64> = HashMap::new();

    // 1. Preferences are always included.
    for item in fetch_preferences(&connection, user_id)? {
        seen.insert(item.id);
        results.push(item);
    }

    // 2./3. FTS5 + (reserved) vector + entity legs.
    if let Some(text) = query.text.as_deref()
        && !text.is_empty()
    {
        for (item, rank) in fts5_search(&connection, query, text, user_id, project_key.as_deref())?
        {
            let rel = 1.0 / (1.0 + rank.abs() / 3.0);
            relevance
                .entry(item.id)
                .and_modify(|current| {
                    if rel > *current {
                        *current = rel;
                    }
                })
                .or_insert(rel);
            fts_ranks.insert(item.id, rank);
            if seen.insert(item.id) {
                results.push(item);
            }
        }
        // Hybrid is requested but no embedding is available on MemoryQuery, so
        // the vector leg is skipped — matching Python when query_embedding=None.
        for item in entity_search(&connection, query, text, user_id, project_key.as_deref())? {
            relevance.entry(item.id).or_insert(ENTITY_RELEVANCE);
            if seen.insert(item.id) {
                results.push(item);
            }
        }
    }

    // 4. Fallback to recent important memories when short of `limit`.
    if results.len() < limit {
        for item in fetch_recent_important(
            &connection,
            query,
            user_id,
            project_key.as_deref(),
            limit.saturating_mul(2),
        )? {
            if seen.insert(item.id) {
                results.push(item);
            }
        }
    }

    // 5. Composite rerank (stable).
    let mut hits: Vec<MemoryHit> = results
        .into_iter()
        .map(|item| {
            let score = score_memory(&item, now_micros, relevance.get(&item.id).copied());
            let fts_rank = fts_ranks.get(&item.id).copied();
            MemoryHit {
                item,
                score,
                fts_rank,
                vector_score: None,
            }
        })
        .collect();
    hits.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(Ordering::Equal)
    });
    hits.truncate(limit);
    Ok(hits)
}

/// Whether the optional sqlite-vec `memory_vec` virtual table is present.
pub fn vector_index_available(store: &LegacyStore) -> Result<bool, StoreError> {
    let connection = store.connection()?;
    crate::migrations::table_exists(&connection, "memory_vec")
}

/// Vector KNN search over the optional sqlite-vec `memory_vec` table.
///
/// The committed baseline schema deliberately omits `memory_vec` (and so does
/// the Python test/dev environment), and this crate never loads a bundled
/// sqlite-vec extension. When the table is absent — or the extension is not
/// available so the query cannot run — this returns an empty result rather than
/// an error, mirroring Python's graceful degradation.
pub fn vector_search(
    store: &LegacyStore,
    actor_id: &str,
    embedding: &[f64],
    limit: usize,
) -> Result<Vec<(i64, f64)>, StoreError> {
    let connection = store.connection()?;
    let user_id = user_id_for(&connection, actor_id)?;
    if !crate::migrations::table_exists(&connection, "memory_vec")? {
        return Ok(Vec::new());
    }
    let payload = serde_json::to_string(embedding)?;
    let query = || -> Result<Vec<(i64, f64)>, StoreError> {
        let mut statement = connection.prepare(
            "SELECT memory_id, distance FROM memory_vec \
             WHERE embedding MATCH ?1 AND k = ?2",
        )?;
        let mut rows = statement.query(params![payload, limit.max(1) as i64])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push((row.get::<_, i64>(0)?, row.get::<_, f64>(1)?));
        }
        Ok(out)
    };
    match query() {
        Ok(rows) => {
            let mut filtered = Vec::new();
            for (memory_id, distance) in rows {
                if let Some(item) = fetch_item(&connection, user_id, memory_id)?
                    && item.status == MEMORY_STATUS_ACTIVE
                {
                    filtered.push((memory_id, distance));
                }
            }
            Ok(filtered)
        }
        Err(_) => Ok(Vec::new()),
    }
}

// --- Lifecycle (pure-SQL parts of lifecycle.py) ---

use crate::domains::memory::{MEMORY_STATUS_ARCHIVED, MEMORY_STATUS_PENDING_CONFIRMATION};

/// Memories whose effective importance drops below this are archived.
const ARCHIVE_THRESHOLD: f64 = 0.1;
/// Decay half-life in days (importance halves after this without access).
const DECAY_HALF_LIFE_DAYS: f64 = 30.0;
/// `Settings.memory_auto_reject_unconfirmed_days` default.
const PENDING_AUTO_REJECT_DAYS: i64 = 14;

fn normalize_timestamp(micros: i64) -> String {
    python_datetime(
        micros.div_euclid(1_000_000),
        micros.rem_euclid(1_000_000) as u32,
    )
}

fn parse_now(now: &str) -> Result<i64, StoreError> {
    timestamp_micros(now)
        .ok_or_else(|| StoreError::InvalidInput(format!("invalid timestamp: {now}")))
}

/// Reduce importance of memories not accessed recently and archive the ones
/// that fall below [`ARCHIVE_THRESHOLD`].
///
/// Formula (Python `lifecycle.run_decay_sweep`):
/// `effective = importance * 1 / (1 + days_since_access / 30)`, using
/// `last_accessed_at` when present and `created_at` otherwise. Pinned
/// memories, preferences, and high-confidence (`>= 0.9`) memories are never
/// archived. Returns the number of archived rows.
pub fn apply_decay(store: &LegacyStore, actor_id: &str, now: &str) -> Result<u64, StoreError> {
    let connection = store.connection()?;
    let user_id = user_id_for(&connection, actor_id)?;
    let now_micros = parse_now(now)?;
    let now_normalized = normalize_timestamp(now_micros);
    let candidates: Vec<(i64, f64, f64, String, Option<String>)> = {
        let mut statement = connection.prepare(
            "SELECT id, importance, confidence, memory_type, \
                    COALESCE(last_accessed_at, created_at) \
             FROM memory_items WHERE user_id = ?1 AND status = ?2 AND pinned = 0",
        )?;
        let mut rows = statement.query(params![user_id, MEMORY_STATUS_ACTIVE])?;
        let mut collected = Vec::new();
        while let Some(row) = rows.next()? {
            collected.push((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ));
        }
        collected
    };
    let mut archived = 0u64;
    for (id, importance, confidence, memory_type, reference) in candidates {
        let Some(reference_micros) = reference.as_deref().and_then(timestamp_micros) else {
            continue;
        };
        let days_since = (now_micros - reference_micros) as f64 / 1_000_000.0 / 86_400.0;
        let effective = importance * (1.0 / (1.0 + days_since / DECAY_HALF_LIFE_DAYS));
        if effective < ARCHIVE_THRESHOLD
            && memory_type != MEMORY_TYPE_PREFERENCE
            && confidence < 0.9
        {
            connection.execute(
                "UPDATE memory_items SET status = ?1, updated_at = ?2 WHERE id = ?3",
                params![MEMORY_STATUS_ARCHIVED, now_normalized, id],
            )?;
            archived += 1;
        }
    }
    Ok(archived)
}

/// Archive memories past their TTL, outside their validity window, or pending
/// confirmation beyond the auto-reject window. Mirrors
/// `run_ttl_sweep` + `run_validity_sweep` + `run_pending_expiry_sweep` and
/// returns the combined number of archived rows. Pinned memories are never
/// expired.
pub fn expire_items(store: &LegacyStore, actor_id: &str, now: &str) -> Result<u64, StoreError> {
    let connection = store.connection()?;
    let user_id = user_id_for(&connection, actor_id)?;
    let now_micros = parse_now(now)?;
    let now_normalized = normalize_timestamp(now_micros);
    let mut archived = 0u64;

    // TTL: archive when now > (valid_from || created_at) + ttl_days.
    let ttl_candidates: Vec<(i64, i64, String)> = {
        let mut statement = connection.prepare(
            "SELECT id, ttl_days, COALESCE(valid_from, created_at) FROM memory_items \
             WHERE user_id = ?1 AND status = ?2 AND ttl_days IS NOT NULL AND pinned = 0",
        )?;
        let mut rows = statement.query(params![user_id, MEMORY_STATUS_ACTIVE])?;
        let mut collected = Vec::new();
        while let Some(row) = rows.next()? {
            collected.push((row.get(0)?, row.get(1)?, row.get(2)?));
        }
        collected
    };
    for (id, ttl_days, reference) in ttl_candidates {
        let Some(reference_micros) = timestamp_micros(&reference) else {
            continue;
        };
        let expiry = reference_micros + ttl_days * 86_400 * 1_000_000;
        if now_micros > expiry {
            connection.execute(
                "UPDATE memory_items SET status = ?1, updated_at = ?2 WHERE id = ?3",
                params![MEMORY_STATUS_ARCHIVED, now_normalized, id],
            )?;
            archived += 1;
        }
    }

    // Validity window: `valid_to < now` (string compare, like Python's SQL).
    archived += connection.execute(
        "UPDATE memory_items SET status = ?1, updated_at = ?2 \
         WHERE user_id = ?3 AND status = ?4 AND valid_to IS NOT NULL AND valid_to < ?5 \
         AND pinned = 0",
        params![
            MEMORY_STATUS_ARCHIVED,
            now_normalized,
            user_id,
            MEMORY_STATUS_ACTIVE,
            now_normalized,
        ],
    )? as u64;

    // Pending confirmation auto-reject.
    let cutoff = normalize_timestamp(now_micros - PENDING_AUTO_REJECT_DAYS * 86_400 * 1_000_000);
    archived += connection.execute(
        "UPDATE memory_items SET status = ?1, updated_at = ?2 \
         WHERE user_id = ?3 AND status = ?4 AND created_at < ?5",
        params![
            MEMORY_STATUS_ARCHIVED,
            now_normalized,
            user_id,
            MEMORY_STATUS_PENDING_CONFIRMATION,
            cutoff,
        ],
    )? as u64;

    Ok(archived)
}
