//! Content-addressed artifact blob store and legacy blob endpoints.
//!
//! `BlobStore` owns the `{root}/{sha[:2]}/{sha}` layout the Python server
//! established (`backend/app/artifacts`), plus the small upload/download
//! surface that does not fit the JSON-RPC protocol (multipart uploads,
//! binary downloads, streamed exports).
//!
//! PDF/DOCX text extraction and PDF/DOCX research exports remain Python-only:
//! they are served by the optional `cool-python-workers` lane and return
//! `worker-unavailable` when it is not running.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cool_store::domains::artifacts::{ARTIFACT_KINDS, Artifact, NewArtifact};
use cool_store::{LegacyStore, StoreError};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// Matches Python `artifact_max_upload_bytes` / `artifact_max_extracted_chars`
/// defaults (backend/app/core/config.py).
pub const MAX_UPLOAD_BYTES: usize = 50_000_000;
pub const MAX_EXTRACTED_CHARS: usize = 100_000;

/// Python `SUPPORTED_IMAGE_TYPES` (backend/app/multimodal.py) — the media
/// types eligible for inline image parts (and `image_analyze` fallback).
pub(crate) const SUPPORTED_IMAGE_TYPES: [&str; 4] =
    ["image/png", "image/jpeg", "image/webp", "image/gif"];

/// Errors the HTTP layer maps to 4xx/5xx responses.
#[derive(Debug)]
pub enum BlobError {
    /// Payload exceeded `MAX_UPLOAD_BYTES` (HTTP 413).
    TooLarge(usize),
    /// Validation failure — empty upload, unknown kind, bad format (HTTP 400).
    Invalid(String),
    /// Store or filesystem failure (HTTP 500).
    Store(StoreError),
    /// Filesystem error (HTTP 500).
    Io(io::Error),
    /// Feature lives on the optional Python worker lane (HTTP 503).
    WorkerUnavailable(&'static str),
}

impl BlobError {
    /// True when the store reported a missing record — callers map it to 404.
    pub fn is_not_found(&self) -> bool {
        matches!(self, Self::Store(StoreError::NotFound(_)))
            || matches!(self, Self::Io(error) if error.kind() == io::ErrorKind::NotFound)
    }
}

impl From<StoreError> for BlobError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

impl From<io::Error> for BlobError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl std::fmt::Display for BlobError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge(size) => write!(
                formatter,
                "File exceeds max upload size ({size} > {MAX_UPLOAD_BYTES} bytes)"
            ),
            Self::Invalid(message) => formatter.write_str(message),
            Self::Store(error) => write!(formatter, "{error}"),
            Self::Io(error) => write!(formatter, "{error}"),
            Self::WorkerUnavailable(feature) => write!(
                formatter,
                "{feature} requires the optional cool-python-workers lane"
            ),
        }
    }
}

impl std::error::Error for BlobError {}

/// Artifact + the absolute path of its blob on disk.
pub struct ArtifactFile {
    pub artifact: Artifact,
    pub path: PathBuf,
}

#[derive(Clone)]
pub struct BlobStore {
    legacy: Arc<LegacyStore>,
    root: PathBuf,
}

impl std::fmt::Debug for BlobStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BlobStore")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

impl BlobStore {
    pub fn new(legacy: Arc<LegacyStore>, root: PathBuf) -> Self {
        Self { legacy, root }
    }

    /// `content_path(sha)` — the two-character fanout path below the root.
    pub fn blob_path(&self, sha256: &str) -> PathBuf {
        self.root.join(&sha256[..2]).join(sha256)
    }

    fn artifact_file(&self, artifact: &Artifact) -> Option<PathBuf> {
        let path = self.root.join(&artifact.storage_path);
        path.exists().then_some(path)
    }

    /// `POST /api/conversations/{id}/artifacts` — multipart upload.
    ///
    /// `filename`/`content` come from the multipart `file` field; `run_id` and
    /// `kind` are query params. Returns the registered artifact row.
    #[allow(clippy::too_many_arguments)]
    pub fn upload(
        &self,
        actor_id: &str,
        conversation_id: i64,
        filename: &str,
        content: &[u8],
        run_id: Option<i64>,
        kind: Option<&str>,
        declared_media_type: Option<&str>,
    ) -> Result<Artifact, BlobError> {
        if content.is_empty() {
            return Err(BlobError::Invalid("Uploaded file is empty".to_owned()));
        }
        if content.len() > MAX_UPLOAD_BYTES {
            return Err(BlobError::TooLarge(content.len()));
        }
        if let Some(value) = kind
            && !ARTIFACT_KINDS.contains(&value)
        {
            return Err(BlobError::Invalid(format!(
                "Invalid kind '{value}'. Must be one of: {ARTIFACT_KINDS:?}"
            )));
        }
        // Python prefers the multipart Content-Type, falling back to a
        // filename guess only when absent or generic.
        let media_type = match declared_media_type {
            Some(value) if !value.is_empty() && value != "application/octet-stream" => {
                value.to_owned()
            }
            _ => media_type_for(filename),
        };
        let kind = match kind {
            Some(value) => value.to_owned(),
            None => infer_kind(filename, &media_type).to_owned(),
        };
        let digest = Sha256::digest(content);
        let sha256_hex = format!("{digest:x}");
        let storage_path = format!("{}/{}", &sha256_hex[..2], sha256_hex);
        let abs_path = self.root.join(&storage_path);
        if !abs_path.exists() {
            if let Some(parent) = abs_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&abs_path, content)?;
        }
        // Text extraction for plain-text files; binary formats (pdf/docx) stay
        // unextracted here — the optional Python worker lane can backfill them.
        let is_text = is_text_file(filename);
        let artifact = self.legacy.register_artifact(
            actor_id,
            conversation_id,
            &NewArtifact {
                filename: filename.to_owned(),
                media_type,
                kind,
                size_bytes: content.len() as i64,
                sha256: Some(sha256_hex),
                storage_path,
                tool_call_id: None,
                parent_id: None,
                metadata: None,
                run_id,
            },
        )?;
        if is_text {
            let mut text = String::from_utf8_lossy(content).into_owned();
            if text.chars().count() > MAX_EXTRACTED_CHARS {
                text = text.chars().take(MAX_EXTRACTED_CHARS).collect();
            }
            self.legacy
                .set_artifact_extracted_text(actor_id, artifact.id, &text)?;
        }
        Ok(artifact)
    }

    /// `GET /api/conversations/{id}/artifacts/{id}/download` — FileResponse.
    ///
    /// `get_artifact` is actor-scoped; the URL's conversation id is checked
    /// against the row here (Python did the same on the ORM object).
    pub fn open_artifact(
        &self,
        actor_id: &str,
        conversation_id: i64,
        artifact_id: i64,
    ) -> Result<ArtifactFile, BlobError> {
        let artifact = self.legacy.get_artifact(actor_id, artifact_id)?;
        if artifact.conversation_id != conversation_id {
            return Err(BlobError::Store(StoreError::NotFound("artifact")));
        }
        let path = self.artifact_file(&artifact).ok_or_else(|| {
            BlobError::Io(io::Error::new(io::ErrorKind::NotFound, "blob file missing"))
        })?;
        Ok(ArtifactFile { artifact, path })
    }

    /// `GET /api/memory/export` — JSON or Markdown snapshot of all memories.
    /// Returns `(body, media_type, attachment_filename)`.
    pub fn export_memories(
        &self,
        actor_id: &str,
        format: &str,
        include_archived: bool,
    ) -> Result<(String, &'static str, String), BlobError> {
        // list_memory_items caps each page at 500; loop for the full export.
        // Python orders by (memory_type, importance desc); apply it after fetch.
        let mut memories = Vec::new();
        let mut offset = 0usize;
        loop {
            let page = self.legacy.list_memory_items(
                actor_id,
                &cool_store::domains::memory::MemoryFilter {
                    status: if include_archived {
                        None
                    } else {
                        Some("active".to_owned())
                    },
                    limit: Some(500),
                    offset,
                    ..Default::default()
                },
            )?;
            let done = page.len() < 500;
            memories.extend(page);
            if done {
                break;
            }
            offset += 500;
        }
        memories.sort_by(|a, b| {
            a.memory_type
                .cmp(&b.memory_type)
                .then(b.importance.total_cmp(&a.importance))
        });
        match format {
            "json" => {
                let items: Vec<Value> = memories
                    .iter()
                    .map(|memory| {
                        json!({
                            "id": memory.id,
                            "type": memory.memory_type,
                            "scope": memory.scope,
                            "content": memory.content,
                            "tags": memory.tags.clone().unwrap_or_else(|| json!([])),
                            "structured": memory.structured,
                            "importance": memory.importance,
                            "confidence": memory.confidence,
                            "source": memory.source,
                            "status": memory.status,
                            "pinned": memory.pinned,
                            "conversation_id": memory.conversation_id,
                            "agent_id": memory.agent_id,
                            "ttl_days": memory.ttl_days,
                            "valid_from": memory.valid_from,
                            "valid_to": memory.valid_to,
                            "created_at": memory.created_at,
                            "updated_at": memory.updated_at,
                        })
                    })
                    .collect();
                // Python `export_memories` returns the structured dump
                // (backend/app/memory/service.py), no `format` wrapper.
                let body = serde_json::to_string_pretty(&json!({
                    "exported_at": cool_store::time::now_python(),
                    "count": items.len(),
                    "memories": items,
                }))
                .map_err(|error| BlobError::Invalid(format!("serialize memory export: {error}")))?;
                Ok((body, "application/json", "memories.json".to_owned()))
            }
            "markdown" | "md" => {
                // Grouped-by-type list (Python `_memories_to_markdown`).
                let mut body = String::from("# Memory export\n\n");
                let mut index = 0usize;
                while index < memories.len() {
                    let memory_type = memories[index].memory_type.clone();
                    let end = memories[index..]
                        .iter()
                        .position(|memory| memory.memory_type != memory_type)
                        .map(|pos| index + pos)
                        .unwrap_or(memories.len());
                    body.push_str(&format!(
                        "## {} ({})\n\n",
                        capitalize(&memory_type),
                        end - index
                    ));
                    for memory in &memories[index..end] {
                        let tags = memory
                            .tags
                            .as_ref()
                            .and_then(|tags| tags.as_array())
                            .filter(|tags| !tags.is_empty())
                            .map(|tags| {
                                format!(
                                    " `{}`",
                                    tags.iter()
                                        .filter_map(|tag| tag.as_str())
                                        .collect::<Vec<_>>()
                                        .join("` `")
                                )
                            })
                            .unwrap_or_default();
                        let pinned = if memory.pinned { " \u{1f4cc}" } else { "" };
                        body.push_str(&format!(
                            "- **{}**{}{}  \n  _importance {:.2} · confidence {:.2} · source `{}` · status `{}`_\n\n",
                            memory.content,
                            tags,
                            pinned,
                            memory.importance,
                            memory.confidence,
                            memory.source,
                            memory.status,
                        ));
                    }
                    index = end;
                }
                Ok((
                    body,
                    "text/markdown; charset=utf-8",
                    "memories.md".to_owned(),
                ))
            }
            other => Err(BlobError::Invalid(format!(
                "format must be 'json' or 'markdown', got '{other}'"
            ))),
        }
    }

    /// `GET /api/research/{id}/export` — the research report as md/html.
    /// pdf/docx live on the optional Python worker lane.
    /// Returns `(body bytes, media_type, attachment_filename)`.
    pub fn export_research(
        &self,
        actor_id: &str,
        run_id: i64,
        format: &str,
    ) -> Result<(Vec<u8>, &'static str, String), BlobError> {
        let run = self.legacy.get_research_run(actor_id, run_id)?;
        // Python 404s when the run has no report yet.
        if run
            .report_markdown
            .as_deref()
            .unwrap_or_default()
            .is_empty()
        {
            return Err(BlobError::Store(StoreError::NotFound("research report")));
        }
        let report = run.report_markdown.unwrap_or_default();
        let base = format!("research-{run_id}");
        match format {
            "md" | "markdown" => Ok((
                report.into_bytes(),
                "text/markdown; charset=utf-8",
                format!("{base}.md"),
            )),
            "html" => Ok((
                markdown_to_html(&report, &run.topic).into_bytes(),
                "text/html; charset=utf-8",
                format!("{base}.html"),
            )),
            "pdf" | "docx" => Err(BlobError::WorkerUnavailable("research export (pdf/docx)")),
            other => Err(BlobError::Invalid(format!(
                "Unsupported export format '{other}' (expected md|html|pdf|docx)"
            ))),
        }
    }
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Minimal extension → MIME map mirroring what Python `mimetypes.guess_type`
/// resolves for the file types this surface handles (unknowns stay
/// `application/octet-stream`, same as Python).
fn media_type_for(filename: &str) -> String {
    let mime = match extension(filename).as_str() {
        ".png" => "image/png",
        ".jpg" | ".jpeg" => "image/jpeg",
        ".gif" => "image/gif",
        ".webp" => "image/webp",
        ".svg" => "image/svg+xml",
        ".bmp" => "image/bmp",
        ".pdf" => "application/pdf",
        ".doc" => "application/msword",
        ".docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        ".odt" => "application/vnd.oasis.opendocument.text",
        ".rtf" => "application/rtf",
        ".py" => "text/x-python",
        ".js" | ".jsx" => "text/javascript",
        ".ts" | ".tsx" => "text/typescript",
        ".rs" => "text/rust",
        ".go" => "text/x-go",
        ".java" => "text/x-java-source",
        ".c" | ".h" => "text/x-c",
        ".cpp" => "text/x-c++",
        ".rb" => "text/x-ruby",
        ".sh" => "application/x-sh",
        ".sql" => "application/sql",
        ".mp3" => "audio/mpeg",
        ".wav" => "audio/wav",
        ".ogg" => "audio/ogg",
        ".flac" => "audio/flac",
        ".m4a" => "audio/mp4",
        ".md" => "text/markdown",
        ".txt" => "text/plain",
        ".json" => "application/json",
        ".yaml" | ".yml" => "text/yaml",
        ".toml" => "application/toml",
        ".ini" | ".cfg" => "text/plain",
        ".csv" => "text/csv",
        ".xml" => "application/xml",
        ".html" => "text/html",
        ".css" => "text/css",
        ".log" | ".env" | ".gitignore" => "text/plain",
        _ => "application/octet-stream",
    };
    mime.to_owned()
}

fn extension(filename: &str) -> String {
    Path::new(filename)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| format!(".{}", ext.to_lowercase()))
        .unwrap_or_default()
}

/// Port of `_EXT_KIND_MAP` + `infer_kind` (backend/app/artifacts/__init__.py).
pub fn infer_kind(filename: &str, media_type: &str) -> &'static str {
    match extension(filename).as_str() {
        ".png" | ".jpg" | ".jpeg" | ".gif" | ".webp" | ".svg" | ".bmp" => "image",
        ".pdf" | ".doc" | ".docx" | ".odt" | ".rtf" => "document",
        ".py" | ".js" | ".ts" | ".tsx" | ".jsx" | ".rs" | ".go" | ".java" | ".c" | ".cpp"
        | ".h" | ".rb" | ".sh" | ".sql" => "code",
        ".mp3" | ".wav" | ".ogg" | ".flac" | ".m4a" => "audio",
        ".md" => "report",
        _ => {
            if media_type.starts_with("image/") {
                "image"
            } else if media_type.starts_with("audio/") {
                "audio"
            } else if media_type.starts_with("video/") || media_type == "application/pdf" {
                "document"
            } else {
                "file"
            }
        }
    }
}

/// Port of `_TEXT_EXTENSIONS` (backend/app/artifacts/__init__.py).
fn is_text_file(filename: &str) -> bool {
    matches!(
        extension(filename).as_str(),
        ".txt"
            | ".py"
            | ".js"
            | ".ts"
            | ".tsx"
            | ".jsx"
            | ".rs"
            | ".go"
            | ".java"
            | ".c"
            | ".cpp"
            | ".h"
            | ".rb"
            | ".sh"
            | ".sql"
            | ".md"
            | ".json"
            | ".yaml"
            | ".yml"
            | ".toml"
            | ".ini"
            | ".cfg"
            | ".csv"
            | ".xml"
            | ".html"
            | ".css"
            | ".log"
            | ".env"
            | ".gitignore"
    )
}

/// Minimal markdown → standalone HTML for research exports, mirroring the
/// Python `_md_to_html`/`_inline_md` helpers (backend/app/research/export.py
/// keeps pdf/docx on the worker lane).
fn markdown_to_html(markdown: &str, title: &str) -> String {
    let mut html = String::with_capacity(markdown.len() * 2);
    html.push_str("<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>");
    html.push_str(&escape_html(title));
    html.push_str("</title><style>body{font-family:ui-sans-serif,system-ui,sans-serif;max-width:52rem;margin:2rem auto;padding:0 1rem;line-height:1.6}code{background:#f3f4f6;padding:.1em .3em;border-radius:4px}pre{background:#f3f4f6;padding:1rem;border-radius:6px;overflow-x:auto}blockquote{border-left:3px solid #d1d5db;margin:0;padding-left:1rem;color:#4b5563}</style></head><body>\n");
    let mut in_code = false;
    let mut in_list = false;
    for line in markdown.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            if in_code {
                html.push_str("</code></pre>\n");
                in_code = false;
            } else {
                if in_list {
                    html.push_str("</ul>\n");
                    in_list = false;
                }
                html.push_str("<pre><code>");
                in_code = true;
            }
            continue;
        }
        if in_code {
            html.push_str(&escape_html(line));
            html.push('\n');
            continue;
        }
        if trimmed.is_empty() {
            if in_list {
                html.push_str("</ul>\n");
                in_list = false;
            }
            continue;
        }
        // Python skips table rows entirely (`_md_to_html`: "skip tables").
        if trimmed.starts_with('|') {
            if in_list {
                html.push_str("</ul>\n");
                in_list = false;
            }
            continue;
        }
        // Headings h1..h6 (Python `^(#{1,6})\s+`).
        let level = trimmed.chars().take_while(|c| *c == '#').count();
        if (1..=6).contains(&level) && trimmed[level..].starts_with(char::is_whitespace) {
            html.push_str(&format!(
                "<h{level}>{}</h{level}>\n",
                inline_md(trimmed[level..].trim())
            ));
        } else if let Some(item) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        {
            if !in_list {
                html.push_str("<ul>\n");
                in_list = true;
            }
            html.push_str(&format!("<li>{}</li>\n", inline_md(item.trim())));
        } else if let Some(quote) = trimmed.strip_prefix("> ") {
            html.push_str(&format!(
                "<blockquote>{}</blockquote>\n",
                inline_md(quote.trim())
            ));
        } else {
            if in_list {
                html.push_str("</ul>\n");
                in_list = false;
            }
            html.push_str(&format!("<p>{}</p>\n", inline_md(trimmed)));
        }
    }
    if in_code {
        html.push_str("</code></pre>\n");
    }
    if in_list {
        html.push_str("</ul>\n");
    }
    html.push_str("</body></html>\n");
    html
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn inline_md(text: &str) -> String {
    let text = escape_html(text);
    // Links [text](url) — Python's regex only links http(s) URLs, so `[n]`
    // citations and javascript:/data: schemes stay literal escaped text.
    let mut out = String::with_capacity(text.len());
    let mut rest = text.as_str();
    while let Some(open) = rest.find('[') {
        let Some(close) = rest[open..].find(']') else {
            break;
        };
        let label = &rest[open + 1..open + close];
        let after = &rest[open + close + 1..];
        let Some(url_and_tail) = after.strip_prefix('(') else {
            // Not a link — emit `[label]` literally and keep scanning.
            out.push_str(&rest[..open + close + 1]);
            rest = after;
            continue;
        };
        let Some(end) = url_and_tail.find(')') else {
            break;
        };
        let url = &url_and_tail[..end];
        out.push_str(&rest[..open]);
        if !label.is_empty() && (url.starts_with("http://") || url.starts_with("https://")) {
            out.push_str(&format!(
                "<a href=\"{}\" rel=\"noopener noreferrer\">{}</a>",
                url, label
            ));
        } else {
            out.push_str(&format!("[{label}]({url})"));
        }
        rest = &url_and_tail[end + 1..];
    }
    out.push_str(rest);
    let out = replace_pairs(&out, "**", "<strong>", "</strong>");
    replace_pairs(&out, "`", "<code>", "</code>")
}

fn replace_pairs(text: &str, marker: &str, open: &str, close: &str) -> String {
    let marker_char = marker.chars().next().unwrap_or('*');
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    let mut in_span = false;
    while let Some(pos) = rest.find(marker) {
        out.push_str(&rest[..pos]);
        out.push_str(if in_span { close } else { open });
        in_span = !in_span;
        rest = &rest[pos + marker.len()..];
    }
    let _ = marker_char;
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infer_kind_matches_python_table() {
        assert_eq!(infer_kind("a.png", "image/png"), "image");
        assert_eq!(infer_kind("a.pdf", "application/pdf"), "document");
        assert_eq!(infer_kind("a.rs", "text/x-rust"), "code");
        assert_eq!(infer_kind("a.mp3", "audio/mpeg"), "audio");
        assert_eq!(infer_kind("a.md", "text/markdown"), "report");
        assert_eq!(infer_kind("a.bin", "application/pdf"), "document");
        assert_eq!(infer_kind("a.bin", "application/octet-stream"), "file");
    }

    #[test]
    fn text_extensions_match_python() {
        assert!(is_text_file("notes.md"));
        assert!(is_text_file("main.rs"));
        assert!(!is_text_file("photo.png"));
        assert!(!is_text_file("doc.pdf"));
    }

    #[test]
    fn markdown_to_html_renders_structure() {
        let html = markdown_to_html("# Title\n\n- a\n- b\n\n> quote\n\n`code`\n", "t");
        assert!(html.contains("<h1>Title</h1>"));
        assert!(html.contains("<li>a</li>"));
        assert!(html.contains("<blockquote>quote</blockquote>"));
        assert!(html.contains("<code>code</code>"));
    }

    #[test]
    fn markdown_to_html_matches_python_headings_and_tables() {
        // Python `^(#{1,6})\s+` supports h5/h6 and skips table rows entirely.
        let html = markdown_to_html(
            "##### deep\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\nplain",
            "t",
        );
        assert!(html.contains("<h5>deep</h5>"));
        assert!(!html.contains("| a | b |"));
        assert!(html.contains("<p>plain</p>"));
    }

    #[test]
    fn inline_md_keeps_tail_and_citations_and_rejects_non_http_links() {
        // Text after the last link is preserved (the pre-fix bug dropped it).
        let html = inline_md("see [site](https://a.b) then tail");
        assert!(html.contains("<a href=\"https://a.b\""));
        assert!(html.ends_with("then tail"));
        // `[n]` citations next to links stay literal.
        let html = inline_md("[1] and [x](https://a.b)");
        assert!(html.contains("[1]"));
        assert!(html.contains("<a href=\"https://a.b\""));
        // Only http(s) links become anchors (Python regex parity).
        let html = inline_md("[e](javascript:alert(1)) and [d](data:text/html,x)");
        assert!(!html.contains("href=\"javascript:"));
        assert!(!html.contains("href=\"data:"));
        assert!(html.contains("[e](javascript:alert(1))"));
    }

    #[test]
    fn export_format_errors() {
        // Worker-gated formats report cleanly. The run row must exist (Python
        // 404s without a report), so seed one through the store.
        let store = LegacyStore::in_memory().expect("store");
        let conversation = store
            .create_conversation(
                crate::local_actor_id().as_str(),
                &cool_store::domains::conversations::NewConversation {
                    title: Some("t".to_owned()),
                    ..Default::default()
                },
            )
            .expect("conversation");
        let run = store
            .create_research_run(
                crate::local_actor_id().as_str(),
                &cool_store::domains::research::NewResearchRun {
                    topic: "topic".to_owned(),
                    depth: 4,
                    model: None,
                    conversation_id: Some(conversation.id),
                    parent_task_run_id: None,
                },
            )
            .expect("run");
        store
            .finish_research_run(
                crate::local_actor_id().as_str(),
                run.id,
                "completed",
                Some("# Report"),
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .expect("finish");
        let blobs = BlobStore::new(Arc::new(store), PathBuf::from("/tmp/unused"));
        let error = blobs
            .export_research(crate::local_actor_id().as_str(), run.id, "pdf")
            .expect_err("pdf is worker-gated");
        assert!(matches!(error, BlobError::WorkerUnavailable(_)));
        let (bytes, media, filename) = blobs
            .export_research(crate::local_actor_id().as_str(), run.id, "md")
            .expect("md export");
        assert_eq!(bytes, b"# Report");
        assert_eq!(media, "text/markdown; charset=utf-8");
        assert!(filename.ends_with(".md"));
    }
}
