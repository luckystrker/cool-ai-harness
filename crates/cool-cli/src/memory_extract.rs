//! LLM memory extraction (M11 WS1d / Workstream B4c).
//!
//! Ports `backend/app/memory/extractor.py` including the best-effort LLM
//! conflict-detection pass: one extraction completion
//! (`EXTRACTION_SYSTEM_PROMPT`, temperature 0.2 / 2000 tokens), then a second
//! completion (`CONFLICT_CHECK_SYSTEM_PROMPT`, temperature 0.1 / 800 tokens)
//! that maps each new fact onto an existing active memory so the stored
//! candidate can carry `supersedes_id`. Candidates land as
//! `source="agent_extraction"` → `pending_confirmation`; a duplicate is updated
//! in place (importance capped at the 0.9 agent ceiling); a high-importance
//! semantic/preference fact mechanically supersedes a 0.45–0.75-overlap
//! same-type neighbour; an episode row is written when the model returns
//! `episode_summary.title`.
//!
//! Recorded divergences from Python:
//! - The extraction model is the runtime's configured model (no
//!   `memory_summary_model` settings override yet) and there is no
//!   `memory_extraction_enabled` / `memory_conflict_check_enabled` gate: the
//!   command always extracts and always runs the conflict pass. Python's
//!   `status="error"` / "No model configured" is therefore never produced —
//!   the CLI always has a configured model.
//! - History is the newest 256 messages (compaction-filtered), not an unbounded
//!   load: the `< 6` gate and the 30-message transcript window only need this
//!   much and the store read stays bounded.
//! - `agent_id` / `run_id` are never set (the Python API path passes `None`
//!   for both, so this is a no-op there; the auto-run hook is unported).
//! - Similarity uses Python's `content.lower().split()` word sets, but the FTS
//!   *candidate query* reuses the store tokenizer (≥3 terms, top-4 longest):
//!   short or punctuation-heavy facts can therefore find different candidates
//!   than Python even though the overlap score itself matches.
//! - `find_duplicate_memory` keeps the first `> 0.7` hit in ascending id
//!   order (Python re-queries by id with no `ORDER BY`); `find_similar_memories`
//!   sorts stably by overlap (Python `list.sort(reverse=True)` is stable).
//! - The conflict pass parses strictly (fence strip + `json.loads`, no `{...}`
//!   fallback) like Python. The fence strip here also accepts the no-newline
//!   ```` ```{...}``` ```` form that Python's strip rejects; the extraction
//!   pass keeps the lenient `{...}` fallback parser.
//! - `llm_error` reports the bare reason in `detail` (Python API parity — the
//!   message is dropped, not logged, a minor divergence).
//! - Episode `importance` is stored unclamped (Python parity).
//! - The third (entity-extraction) LLM pass is deferred: the Rust store has no
//!   entity upsert (`create_entity` conflicts on an existing name).
//! - Provider routing has no `ResilientProvider` fallback chain (one
//!   `ModelDriver`, as configured by the CLI).
//! - The API-layer `< 4` pre-check is folded into the extractor's `< 6` gate
//!   (a stricter skip; Python would run the extractor and skip inside it).

use std::sync::Arc;

use async_trait::async_trait;
use cool_agent::{Message, MessageRole, ModelDriver, ModelEvent, ModelRequest, ProviderError};
use cool_app_server::{MemoryExtractError, MemoryExtractor};
use cool_protocol::MemoryExtractResult;
use cool_store::LegacyStore;
use cool_store::StoreError;
use cool_store::domains::memory::{
    MEMORY_TYPE_PREFERENCE, MEMORY_TYPE_PROCEDURAL, MEMORY_TYPE_SEMANTIC, MemoryItemPatch,
    NewEpisode, NewMemoryItem, SCOPE_GLOBAL,
};
use serde_json::Value;

/// `bucket -> (content -> superseded memory id)` from the conflict pass.
type SupersedesMap = std::collections::HashMap<String, std::collections::HashMap<String, i64>>;
/// One conflict-pass batch: `(bucket, content)`.
type ConflictBatch = (&'static str, String);

const MIN_MESSAGES_FOR_EXTRACTION: usize = 6;
const TRANSCRIPT_MAX_CHARS: usize = 8_000;
const TRANSCRIPT_MAX_MESSAGES: usize = 30;
const TOOL_OUTPUT_CHARS: usize = 300;
const OTHER_OUTPUT_CHARS: usize = 500;
const PROMPT_PREVIEW_CHARS: usize = 200;
/// Newest history window loaded from the store (bounded read; see module docs).
const HISTORY_WINDOW: usize = 256;
const EXTRACTION_TEMPERATURE: f32 = 0.2;
const EXTRACTION_MAX_TOKENS: u32 = 2_000;
const CONFLICT_TEMPERATURE: f32 = 0.1;
const CONFLICT_MAX_TOKENS: u32 = 800;

const EXTRACTION_SYSTEM_PROMPT: &str = "\
You are a memory extraction system. Analyze the conversation below and extract durable memories that would be useful in future sessions.

Return a JSON object with this exact structure:
{
  \"user_preferences\": [
    {\"content\": \"...\", \"importance\": 0.0-1.0, \"confidence\": 0.0-1.0}
  ],
  \"project_facts\": [
    {\"content\": \"...\", \"importance\": 0.0-1.0, \"confidence\": 0.0-1.0}
  ],
  \"procedures\": [
    {\"content\": \"...\", \"importance\": 0.0-1.0, \"confidence\": 0.0-1.0}
  ],
  \"episode_summary\": {
    \"title\": \"Brief title of what happened\",
    \"summary\": \"2-3 sentence summary of the interaction\",
    \"outcome\": \"success|failure|partial|unknown\",
    \"importance\": 0.0-1.0
  }
}

Rules:
- Only extract information useful in FUTURE sessions.
- Do NOT store temporary debug details, one-time commands, or secrets.
- Do NOT store information already obvious from the project structure.
- Include confidence based on how explicitly the information was stated.
- user_preferences: explicit or strongly implied user preferences.
- project_facts: non-obvious facts about the project, stack, or architecture.
- procedures: reusable how-to knowledge (commands, workflows, patterns).
- episode_summary: always provide a brief summary of what happened.
- If nothing is worth remembering, return empty arrays and a minimal episode.
- Return ONLY valid JSON, no markdown fences or extra text.
";

const CONFLICT_CHECK_SYSTEM_PROMPT: &str = "\
You compare newly extracted facts against existing memories. For each new fact, decide whether it contradicts/updates an existing memory (same topic, different or corrected content) or merely complements it.

Existing memories are given as: id: \"<content>\".
New facts are indexed 0..N-1.

Return ONLY valid JSON:
{\"results\": [{\"index\": 0, \"conflict_with\": null | <existing id>, \"kind\": \"contradicts|updates|complements|duplicate\"}]}

Rules:
- \"contradicts\"/\"updates\": the new fact replaces or corrects the existing one.
- \"complements\": adds new information, no conflict.
- \"duplicate\": same meaning as an existing memory (should be merged, not kept).
- If unsure, prefer \"complements\" with conflict_with null.
- No markdown fences, no extra text.
";

/// CLI memory extractor: legacy store + one configured model driver.
pub struct CliMemoryExtractor {
    store: Option<Arc<LegacyStore>>,
    provider: Arc<dyn ModelDriver>,
    model: String,
}

impl CliMemoryExtractor {
    pub fn new(
        store: Option<Arc<LegacyStore>>,
        provider: Arc<dyn ModelDriver>,
        model: String,
    ) -> Self {
        Self {
            store,
            provider,
            model,
        }
    }
}

#[async_trait]
impl MemoryExtractor for CliMemoryExtractor {
    async fn extract(
        &self,
        actor: &str,
        conversation_id: i64,
        idempotency_key: &str,
    ) -> Result<MemoryExtractResult, MemoryExtractError> {
        let Some(store) = self.store.clone() else {
            return Err(MemoryExtractError::Unavailable(
                "memory store unavailable".to_owned(),
            ));
        };
        let actor_id = actor.to_owned();
        let provider = self.provider.clone();
        let model = self.model.clone();
        // Fingerprint covers only the target id (the sole varying input).
        let fingerprint = format!(r#"{{"id":{conversation_id}}}"#);
        let store_for_action = store.clone();
        let actor_for_action = actor_id.clone();
        store
            .run_idempotent_async(
                &actor_id,
                "memory.extract",
                idempotency_key,
                &fingerprint,
                move || {
                    let store = store_for_action.clone();
                    let actor = actor_for_action.clone();
                    let provider = provider.clone();
                    let model = model.clone();
                    async move {
                        extract_memories(&store, &actor, conversation_id, provider.as_ref(), &model)
                            .await
                    }
                },
            )
            .await
            .map(|idempotent| idempotent.value)
            .map_err(|error| match error {
                StoreError::Conflict(message) => MemoryExtractError::Conflict(message),
                other => MemoryExtractError::Failed(other.to_string()),
            })
    }
}

fn skipped(reason: &str) -> MemoryExtractResult {
    MemoryExtractResult {
        status: "skipped".to_owned(),
        stored_count: 0,
        detail: Some(reason.to_owned()),
    }
}

fn completed(stored_count: i64) -> MemoryExtractResult {
    MemoryExtractResult {
        status: "completed".to_owned(),
        stored_count,
        detail: None,
    }
}

/// Python `message_text`: the string content (the Rust `Message.content` is
/// already text-only).
fn message_text(content: Option<&str>) -> String {
    content.unwrap_or("").to_owned()
}

/// Python `_build_transcript`: drop system, keep the last 30, truncate tool
/// outputs to 300 and other messages to 500, stop at an 8000-char budget.
fn build_transcript(messages: &[cool_store::domains::conversations::Message]) -> String {
    let relevant: Vec<_> = messages
        .iter()
        .filter(|message| message.role != "system")
        .collect();
    let start = relevant.len().saturating_sub(TRANSCRIPT_MAX_MESSAGES);
    let mut lines: Vec<String> = Vec::new();
    let mut total_chars = 0usize;
    for message in &relevant[start..] {
        let role = message.role.as_str();
        let mut content = message_text(message.content.as_deref());
        if role == "tool" && content.chars().count() > TOOL_OUTPUT_CHARS {
            let head: String = content.chars().take(TOOL_OUTPUT_CHARS).collect();
            content = format!("{head}… (truncated)");
        } else if content.chars().count() > OTHER_OUTPUT_CHARS {
            let head: String = content.chars().take(OTHER_OUTPUT_CHARS).collect();
            content = format!("{head}…");
        }
        let line = format!("{role}: {content}");
        if total_chars + line.chars().count() > TRANSCRIPT_MAX_CHARS {
            break;
        }
        total_chars += line.chars().count();
        lines.push(line);
    }
    lines.join("\n")
}

/// Strip markdown fences, else find the outermost `{...}` block.
fn parse_json_block(content: &str) -> Option<Value> {
    let trimmed = content.trim();
    let mut text = trimmed.to_owned();
    if text.starts_with("```") {
        let mut lines = text.split('\n').collect::<Vec<_>>();
        if lines.first().is_some_and(|line| line.starts_with("```")) {
            lines.remove(0);
        }
        if lines.last().is_some_and(|line| line.trim() == "```") {
            lines.pop();
        }
        text = lines.join("\n");
    }
    if let Ok(value) = serde_json::from_str::<Value>(&text) {
        return Some(value);
    }
    let start = text.find('{')?;
    let end = text.rfind('}')? + 1;
    if end <= start {
        return None;
    }
    serde_json::from_str::<Value>(&text[start..end]).ok()
}

/// Python's conflict pass parses strictly (`json.loads` after a fence strip,
/// no `{...}` fallback): garbage that happens to contain braces is a parse
/// failure, and the pass degrades to an empty supersedes map.
fn parse_conflict_json(content: &str) -> Option<Value> {
    let raw = content.trim();
    let raw = raw
        .strip_prefix("```")
        .map(|rest| {
            let rest = rest.trim_start_matches("json");
            rest.split_once('\n').map(|(_, tail)| tail).unwrap_or(rest)
        })
        .unwrap_or(raw);
    let raw = raw.trim_end().strip_suffix("```").unwrap_or(raw).trim();
    serde_json::from_str::<Value>(raw).ok()
}

/// One-shot completion: fold `ModelEvent::Content` until `Finish`.
async fn complete(
    provider: &dyn ModelDriver,
    model: &str,
    system: &str,
    user: &str,
    temperature: f32,
    max_tokens: u32,
) -> Result<String, ProviderError> {
    let request = ModelRequest {
        model: model.to_owned(),
        messages: vec![
            Message::text(MessageRole::System, system),
            Message::text(MessageRole::User, user),
        ],
        tools: Vec::new(),
        temperature,
        max_tokens: Some(max_tokens),
    };
    let mut stream = provider.stream(request).await?;
    let mut content = String::new();
    while let Some(event) = {
        use futures_util::StreamExt as _;
        stream.next().await
    } {
        match event? {
            ModelEvent::Content(text) => content.push_str(&text),
            ModelEvent::Finish { .. } => break,
            _ => {}
        }
    }
    Ok(content)
}

struct Candidate {
    bucket: &'static str,
    content: String,
    importance: f64,
    confidence: f64,
    memory_type: &'static str,
}

fn bucket_candidates(parsed: &Value) -> Vec<Candidate> {
    let mut candidates = Vec::new();
    // (JSON key, Python bucket name, memory_type, default importance, default confidence)
    let buckets: [(&str, &'static str, &'static str, f64, f64); 3] = [
        (
            "user_preferences",
            "preference",
            MEMORY_TYPE_PREFERENCE,
            0.7,
            0.7,
        ),
        ("project_facts", "fact", MEMORY_TYPE_SEMANTIC, 0.5, 0.6),
        ("procedures", "procedure", MEMORY_TYPE_PROCEDURAL, 0.5, 0.6),
    ];
    for (key, bucket, memory_type, default_importance, default_confidence) in buckets {
        let Some(items) = parsed.get(key).and_then(Value::as_array) else {
            continue;
        };
        for item in items {
            let content = item.get("content").and_then(Value::as_str).unwrap_or("");
            if content.trim().is_empty() {
                continue;
            }
            candidates.push(Candidate {
                bucket,
                content: content.to_owned(),
                importance: item
                    .get("importance")
                    .and_then(Value::as_f64)
                    .unwrap_or(default_importance),
                confidence: item
                    .get("confidence")
                    .and_then(Value::as_f64)
                    .unwrap_or(default_confidence),
                memory_type,
            });
        }
    }
    candidates
}

/// LLM conflict-detection pass. Returns `bucket -> (content -> superseded id)`.
/// Best-effort: a parse/LLM failure returns an empty map (Python parity — the
/// mechanical supersede path still runs).
async fn detect_conflicts(
    store: &LegacyStore,
    actor: &str,
    provider: &dyn ModelDriver,
    model: &str,
    candidates: &[Candidate],
) -> SupersedesMap {
    let mut supersedes = SupersedesMap::new();
    if candidates.is_empty() {
        return supersedes;
    }
    let mut batches: Vec<ConflictBatch> = Vec::new();
    let mut seen_ids: Vec<i64> = Vec::new();
    let mut existing_lines: Vec<String> = Vec::new();
    let mut new_facts_lines: Vec<String> = Vec::new();
    for candidate in candidates {
        // Python wraps the FTS probe in try/except and degrades to no
        // candidates rather than aborting the extraction.
        let similar = store
            .find_similar_memories(actor, &candidate.content, 0.3, 3, Some("active"), None)
            .unwrap_or_default();
        for (item, _overlap) in similar {
            if !seen_ids.contains(&item.id) {
                seen_ids.push(item.id);
                let preview: String = item.content.chars().take(PROMPT_PREVIEW_CHARS).collect();
                existing_lines.push(format!("{}: \"{}\"", item.id, preview));
            }
        }
        let index = batches.len();
        let preview: String = candidate
            .content
            .chars()
            .take(PROMPT_PREVIEW_CHARS)
            .collect();
        new_facts_lines.push(format!("{index}: \"{}\"", preview));
        batches.push((candidate.bucket, candidate.content.clone()));
    }
    let prompt = format!(
        "Existing memories:\n{}\n\nNew facts:\n{}",
        if existing_lines.is_empty() {
            "(none)".to_owned()
        } else {
            existing_lines.join("\n")
        },
        new_facts_lines.join("\n")
    );
    let raw = match complete(
        provider,
        model,
        CONFLICT_CHECK_SYSTEM_PROMPT,
        &prompt,
        CONFLICT_TEMPERATURE,
        CONFLICT_MAX_TOKENS,
    )
    .await
    {
        Ok(raw) => raw,
        Err(_) => return supersedes,
    };
    let Some(parsed) = parse_conflict_json(&raw) else {
        return supersedes;
    };
    let Some(results) = parsed.get("results").and_then(Value::as_array) else {
        return supersedes;
    };
    for row in results {
        let Some(index) = row.get("index").and_then(Value::as_u64) else {
            continue;
        };
        let index = index as usize;
        if index >= batches.len() {
            continue;
        }
        let kind = row.get("kind").and_then(Value::as_str).unwrap_or("");
        if kind != "contradicts" && kind != "updates" {
            continue;
        }
        let Some(conflict_id) = row.get("conflict_with").and_then(Value::as_i64) else {
            continue;
        };
        if !seen_ids.contains(&conflict_id) {
            continue;
        }
        let (bucket, content) = &batches[index];
        supersedes
            .entry((*bucket).to_owned())
            .or_default()
            .insert(content.clone(), conflict_id);
    }
    supersedes
}

async fn extract_memories(
    store: &LegacyStore,
    actor: &str,
    conversation_id: i64,
    provider: &dyn ModelDriver,
    model: &str,
) -> Result<MemoryExtractResult, StoreError> {
    // Bounded newest window; the compaction pointer drops covered messages.
    let recent = store.recent_messages(actor, conversation_id, HISTORY_WINDOW)?;
    let mut messages = recent.messages;
    if let Ok(Some(working)) = store.get_working_memory(actor, conversation_id)
        && let Some(up_to) = working.summary_up_to_message_id
    {
        messages.retain(|message| message.id > up_to);
    }
    if messages.len() < MIN_MESSAGES_FOR_EXTRACTION {
        return Ok(skipped("too_few_messages"));
    }
    let transcript = build_transcript(&messages);
    if transcript.is_empty() {
        return Ok(skipped("empty_transcript"));
    }
    let user_prompt = format!("Conversation transcript:\n\n{transcript}");
    let raw = match complete(
        provider,
        model,
        EXTRACTION_SYSTEM_PROMPT,
        &user_prompt,
        EXTRACTION_TEMPERATURE,
        EXTRACTION_MAX_TOKENS,
    )
    .await
    {
        Ok(raw) => raw,
        Err(_) => {
            return Ok(MemoryExtractResult {
                status: "skipped".to_owned(),
                stored_count: 0,
                // Python's API `detail` is the bare reason; the message is
                // only logged.
                detail: Some("llm_error".to_owned()),
            });
        }
    };
    let Some(parsed) = parse_json_block(&raw) else {
        return Ok(skipped("parse_error"));
    };
    let candidates = bucket_candidates(&parsed);
    let supersedes = detect_conflicts(store, actor, provider, model, &candidates).await;

    let mut stored_count = 0i64;
    for candidate in &candidates {
        let status = "pending_confirmation";
        let supersede = supersedes
            .get(candidate.bucket)
            .and_then(|map| map.get(&candidate.content))
            .copied();
        // Python `_find_duplicate`: exact content then FTS overlap > 0.7 in the
        // same type/status tier; a duplicate is updated in place (importance
        // capped at the 0.9 agent ceiling, Python `remember` clamps first).
        if let Some(existing) = store
            .find_duplicate_memory(actor, candidate.memory_type, status, &candidate.content)
            .unwrap_or(None)
        {
            let capped = candidate.importance.clamp(0.0, 0.9);
            let _ = store.update_memory_item(
                actor,
                existing.id,
                &MemoryItemPatch {
                    content: Some(candidate.content.clone()),
                    importance: Some(existing.importance.max(capped)),
                    confidence: Some(candidate.confidence),
                    ..MemoryItemPatch::default()
                },
            );
            stored_count += 1;
            continue;
        }
        // Mechanical supersede (Python C3): high-importance semantic/preference
        // facts pick a same-type 0.45–0.75-overlap neighbour when the LLM found
        // none. A store failure degrades to "no supersede" (Python try/except).
        let mut supersedes_id = supersede;
        if supersedes_id.is_none()
            && candidate.importance >= 0.8
            && matches!(
                candidate.memory_type,
                MEMORY_TYPE_SEMANTIC | MEMORY_TYPE_PREFERENCE
            )
        {
            let neighbours = store
                .find_similar_memories(
                    actor,
                    &candidate.content,
                    0.45,
                    5,
                    Some("active"),
                    Some(candidate.memory_type),
                )
                .unwrap_or_default();
            supersedes_id = neighbours
                .into_iter()
                .find(|(_, overlap)| *overlap < 0.75)
                .map(|(item, _)| item.id);
        }
        let _ = store.create_memory_item(
            actor,
            &NewMemoryItem {
                scope: SCOPE_GLOBAL.to_owned(),
                conversation_id: Some(conversation_id),
                memory_type: candidate.memory_type.to_owned(),
                content: candidate.content.clone(),
                importance: candidate.importance.clamp(0.0, 0.9),
                confidence: candidate.confidence.clamp(0.0, 1.0),
                source: "agent_extraction".to_owned(),
                supersedes_id,
                ..NewMemoryItem::default()
            },
        )?;
        stored_count += 1;
    }

    // Optional episode (Python writes one whenever `episode_summary.title`).
    if let Some(episode) = parsed.get("episode_summary")
        && episode
            .get("title")
            .and_then(Value::as_str)
            .is_some_and(|title| !title.is_empty())
    {
        let _ = store.create_episode(
            actor,
            &NewEpisode {
                agent_id: None,
                conversation_id: Some(conversation_id),
                run_id: None,
                title: episode
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                summary: episode
                    .get("summary")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                outcome: episode
                    .get("outcome")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_owned(),
                importance: episode
                    .get("importance")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.5),
                tags: None,
                related_entities: None,
                started_at: None,
                ended_at: None,
            },
        )?;
    }

    Ok(completed(stored_count))
}

#[cfg(test)]
mod tests {
    use super::*;
    use cool_agent::ScriptedDriver;
    use serde_json::json;

    #[test]
    fn transcript_drops_system_and_truncates() {
        let messages = vec![
            message("system", &"sys".repeat(100)),
            message("user", &"u".repeat(600)),
            message("tool", &"t".repeat(400)),
            message("assistant", "ok"),
        ];
        let transcript = build_transcript(&messages);
        assert!(!transcript.contains("system:"), "system messages dropped");
        let user_line = transcript
            .lines()
            .find(|line| line.starts_with("user:"))
            .expect("user line");
        assert!(
            user_line.ends_with('…'),
            "long user text truncated: {user_line}"
        );
        let tool_line = transcript
            .lines()
            .find(|line| line.starts_with("tool:"))
            .expect("tool line");
        assert!(
            tool_line.ends_with("… (truncated)"),
            "tool output marker: {tool_line}"
        );
    }

    #[test]
    fn transcript_keeps_the_newest_within_budget() {
        let mut messages = Vec::new();
        for index in 0..40 {
            messages.push(message(
                "user",
                &format!("message number {index} padding padding"),
            ));
        }
        let transcript = build_transcript(&messages);
        assert!(
            transcript.contains("message number 39"),
            "newest message retained"
        );
        let lines = transcript.lines().count();
        assert!(lines <= TRANSCRIPT_MAX_MESSAGES, "window bound: {lines}");
    }

    #[test]
    fn parse_json_block_strips_fences_and_finds_braces() {
        let fenced = "```json\n{\"a\": 1}\n```";
        assert_eq!(parse_json_block(fenced).unwrap()["a"], 1);
        let noisy = "Here you go:\n{\"b\": 2}\nDone.";
        assert_eq!(parse_json_block(noisy).unwrap()["b"], 2);
        assert!(parse_json_block("no json here").is_none());
    }

    #[test]
    fn conflict_parser_is_strict_and_accepts_the_no_newline_fence() {
        // Strict: no `{...}` fallback for garbage that happens to contain braces.
        assert!(parse_conflict_json("garbage {\"a\": 1} more").is_none());
        // No-newline fence form (Python's strip rejects this; Rust accepts).
        assert_eq!(
            parse_conflict_json("```{\"results\":[]}```").unwrap()["results"],
            serde_json::json!([])
        );
        // Ordinary fenced block.
        assert!(parse_conflict_json("```json\n{\"results\":[]}\n```").is_some());
    }

    #[test]
    fn bucket_candidates_map_types_and_defaults() {
        let parsed = json!({
            "user_preferences": [{"content": "likes dark mode", "importance": 0.9}],
            "project_facts": [{"content": "uses sqlite", "confidence": 0.4}],
            "procedures": [{"content": "run cargo test", "importance": 0.2, "confidence": 0.9}],
            "episode_summary": {"title": "setup"}
        });
        let candidates = bucket_candidates(&parsed);
        assert_eq!(candidates.len(), 3);
        assert_eq!(candidates[0].memory_type, MEMORY_TYPE_PREFERENCE);
        assert_eq!(candidates[0].importance, 0.9, "model value wins");
        assert_eq!(candidates[0].confidence, 0.7, "bucket default");
        assert_eq!(candidates[1].memory_type, MEMORY_TYPE_SEMANTIC);
        assert_eq!(candidates[1].importance, 0.5);
        assert_eq!(candidates[2].memory_type, MEMORY_TYPE_PROCEDURAL);
    }

    #[tokio::test]
    async fn extraction_skips_short_conversations() {
        let directory = tempfile::tempdir().unwrap();
        let store = test_store(directory.path());
        let driver = Arc::new(ScriptedDriver::echo());
        let extractor = CliMemoryExtractor::new(Some(store.clone()), driver, "scripted".into());
        let result = extractor
            .extract("local-user", 1, "key-1")
            .await
            .expect("extract");
        assert_eq!(result.status, "skipped");
        assert_eq!(result.detail.as_deref(), Some("too_few_messages"));
    }

    #[tokio::test]
    async fn extraction_stores_candidates_and_runs_the_conflict_pass() {
        let directory = tempfile::tempdir().unwrap();
        let store = test_store(directory.path());
        for index in 0..6 {
            store
                .add_message(
                    "local-user",
                    1,
                    &cool_store::domains::conversations::NewMessage {
                        role: "user".to_owned(),
                        content: Some(format!("I prefer tabs over spaces, note {index}")),
                        ..Default::default()
                    },
                )
                .expect("seed message");
        }
        // Seed one ACTIVE neighbour so the conflict pass has a candidate.
        let neighbour = store
            .create_memory_item(
                "local-user",
                &NewMemoryItem {
                    memory_type: MEMORY_TYPE_PREFERENCE.to_owned(),
                    content: "I prefer tabs over spaces".to_owned(),
                    source: "user_explicit".to_owned(),
                    confirmed: true,
                    ..NewMemoryItem::default()
                },
            )
            .expect("neighbour");

        let extraction_json = json!({
            "user_preferences": [{"content": "I prefer tabs over spaces", "importance": 0.8, "confidence": 0.9}],
            "project_facts": [],
            "procedures": [],
            "episode_summary": {"title": "Style chat", "summary": "Talked about tabs.", "outcome": "success", "importance": 0.4}
        })
        .to_string();
        let conflict_json = json!({
            "results": [{"index": 0, "conflict_with": neighbour.id, "kind": "updates"}]
        })
        .to_string();
        let driver = Arc::new(ScriptedDriver::new([
            Ok(vec![
                ModelEvent::Content(extraction_json),
                ModelEvent::Finish {
                    reason: Some("stop".to_owned()),
                },
            ]),
            Ok(vec![
                ModelEvent::Content(conflict_json),
                ModelEvent::Finish {
                    reason: Some("stop".to_owned()),
                },
            ]),
        ]));
        let extractor =
            CliMemoryExtractor::new(Some(store.clone()), driver.clone(), "scripted".into());
        let result = extractor
            .extract("local-user", 1, "key-2")
            .await
            .expect("extract");
        assert_eq!(result.status, "completed");
        assert_eq!(result.stored_count, 1);

        let pending = store
            .list_pending_items("local-user", None)
            .expect("pending");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].source, "agent_extraction");
        assert_eq!(pending[0].status, "pending_confirmation");
        assert_eq!(
            pending[0].supersedes_id,
            Some(neighbour.id),
            "the conflict pass wires supersedes_id"
        );

        let episodes = store
            .list_episodes("local-user", Some(10))
            .expect("episodes");
        assert_eq!(episodes.len(), 1);
        assert_eq!(episodes[0].title, "Style chat");

        let requests = driver.requests().await;
        assert_eq!(requests.len(), 2, "extraction + conflict completions");
        assert_eq!(requests[0].temperature, 0.2);
        assert_eq!(requests[0].max_tokens, Some(2_000));
        assert_eq!(requests[1].temperature, 0.1);
        assert_eq!(requests[1].max_tokens, Some(800));
    }

    fn test_store(path: &std::path::Path) -> Arc<LegacyStore> {
        let database = path.join("harness.db");
        let store = LegacyStore::open(
            &database,
            &cool_store::StoreOptions {
                initialize_if_missing: true,
                ..cool_store::StoreOptions::default()
            },
        )
        .expect("store");
        store.ensure_actor("local-user").expect("actor");
        let conversation = store
            .create_conversation(
                "local-user",
                &cool_store::domains::conversations::NewConversation {
                    title: Some("extract".to_owned()),
                    ..Default::default()
                },
            )
            .expect("conversation");
        assert_eq!(conversation.id, 1, "first conversation is id 1");
        Arc::new(store)
    }

    fn message(role: &str, content: &str) -> cool_store::domains::conversations::Message {
        cool_store::domains::conversations::Message {
            id: 0,
            conversation_id: 1,
            role: role.to_owned(),
            content: Some(content.to_owned()),
            tool_calls: None,
            tool_result: None,
            usage: None,
            thinking: None,
            model: None,
            duration_ms: None,
            artifact_ids: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }
}
