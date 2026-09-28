use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use cool_security::{
    Capability, CapabilityPolicy, Decision, Workspace, mask_json, mask_secrets,
    sanitize_environment,
};
use globset::{Glob, GlobMatcher};
use regex::Regex;
use serde_json::{Value, json};
use tokio::time::timeout;
use uuid::Uuid;

use crate::loop_runtime::CancelSignal;

#[derive(Clone, Debug)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

/// One tool as surfaced by the runtime's catalog (agent-constructor /
/// subagent tool pickers). `dangerous` marks a tool whose default decision is
/// `Ask`, i.e. one that requires an approval before it can run.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolCatalogEntry {
    pub name: String,
    pub description: String,
    pub parameters: Value,
    pub capabilities: Vec<String>,
    pub dangerous: bool,
}

/// Stable wire name for a capability, matching the Python `Capability.value`.
pub fn capability_name(capability: Capability) -> &'static str {
    match capability {
        Capability::Read => "read",
        Capability::Write => "write",
        Capability::Execute => "execute",
        Capability::Network => "network",
        Capability::Git => "git",
        Capability::SendExternal => "send_external",
    }
}

#[derive(Clone, Debug)]
pub struct ToolResult {
    /// `output` carries the bounded (possibly head/tail-trimmed) view; `truncated`
    /// marks that a fuller body was left behind, e.g. spilled to `.cool/spill/`.
    pub output: Value,
    pub is_error: bool,
    pub error_code: Option<String>,
    pub truncated: bool,
}

impl ToolResult {
    pub fn ok(output: Value) -> Self {
        Self {
            output,
            is_error: false,
            error_code: None,
            truncated: false,
        }
    }

    pub fn error(code: impl Into<String>, message: impl Into<String>) -> Self {
        let code = code.into();
        Self {
            output: json!({"error": message.into()}),
            is_error: true,
            error_code: Some(code),
            truncated: false,
        }
    }

    pub fn masked(mut self) -> Self {
        mask_json(&mut self.output);
        self
    }
}

#[derive(Debug)]
pub enum ToolError {
    UnknownTool(String),
    InvalidArguments(String),
    Security(String),
    Io(std::io::Error),
    Timeout,
    Cancelled,
}

impl fmt::Display for ToolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownTool(name) => write!(formatter, "unknown tool: {name}"),
            Self::InvalidArguments(reason) => write!(formatter, "invalid arguments: {reason}"),
            Self::Security(reason) => write!(formatter, "security policy rejected tool: {reason}"),
            Self::Io(error) => write!(formatter, "tool I/O error: {error}"),
            Self::Timeout => formatter.write_str("tool timed out"),
            Self::Cancelled => formatter.write_str("tool was cancelled"),
        }
    }
}

impl std::error::Error for ToolError {}

impl From<std::io::Error> for ToolError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

#[derive(Clone)]
pub struct ToolContext {
    pub workspace: Workspace,
    pub policy: CapabilityPolicy,
    pub timeout: Duration,
    pub max_output_bytes: usize,
    pub environment: HashMap<String, String>,
    pub allowed_secret_environment: BTreeSet<String>,
    /// Explicit opt-in for a single-user trusted-host launcher. Production
    /// embeddings keep this false unless they supply an OS-isolated worker.
    pub allow_trusted_host_processes: bool,
    pub cancel: Option<CancelSignal>,
    /// Server-derived actor for store-backed tools. Never read from tool
    /// arguments.
    pub actor_id: String,
    /// Conversation bound to the run, when known; store-backed tools that are
    /// conversation-scoped use it.
    pub conversation_id: Option<i64>,
    /// Provider tool-call id of the current invocation; the runtime stamps it
    /// so artifacts like `.cool/spill/{call_id}-stdout.txt` correlate with the
    /// call in the event log.
    pub call_id: Option<String>,
}

impl ToolContext {
    pub fn new(workspace: Workspace, policy: CapabilityPolicy) -> Self {
        Self {
            workspace,
            policy,
            timeout: Duration::from_secs(30),
            max_output_bytes: 1_048_576,
            environment: HashMap::new(),
            allowed_secret_environment: BTreeSet::new(),
            allow_trusted_host_processes: false,
            cancel: None,
            actor_id: "local-user".to_owned(),
            conversation_id: None,
            call_id: None,
        }
    }

    /// Sets the server-derived actor that store-backed tools scope their reads
    /// and writes to.
    pub fn with_actor(mut self, actor_id: impl Into<String>) -> Self {
        self.actor_id = actor_id.into();
        self
    }

    /// Binds the run's conversation so conversation-scoped store tools resolve
    /// the right rows.
    pub fn with_conversation(mut self, conversation_id: Option<i64>) -> Self {
        self.conversation_id = conversation_id;
        self
    }
}

#[async_trait]
pub trait ToolHandler: Send + Sync {
    async fn execute(
        &self,
        context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolResult, ToolError>;
}

#[derive(Clone)]
pub struct Tool {
    pub definition: ToolDefinition,
    pub capabilities: BTreeSet<Capability>,
    pub default_decision: Decision,
    handler: Arc<dyn ToolHandler>,
}

impl Tool {
    pub fn new(
        definition: ToolDefinition,
        capabilities: impl IntoIterator<Item = Capability>,
        default_decision: Decision,
        handler: impl ToolHandler + 'static,
    ) -> Self {
        Self {
            definition,
            capabilities: capabilities.into_iter().collect(),
            default_decision,
            handler: Arc::new(handler),
        }
    }

    pub async fn execute(
        &self,
        context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolResult, ToolError> {
        self.handler.execute(context, arguments).await
    }
}

/// Shared, dynamically updatable tool registry. Clones share the same backing
/// map, so a host that registers tools at runtime (e.g. operator MCP servers
/// connecting after startup) is visible to every live `AgentRuntime`.
#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: Arc<RwLock<BTreeMap<String, Tool>>>,
}

fn read_tools(
    tools: &RwLock<BTreeMap<String, Tool>>,
) -> std::sync::RwLockReadGuard<'_, BTreeMap<String, Tool>> {
    tools.read().unwrap_or_else(|poison| poison.into_inner())
}

fn write_tools(
    tools: &RwLock<BTreeMap<String, Tool>>,
) -> std::sync::RwLockWriteGuard<'_, BTreeMap<String, Tool>> {
    tools.write().unwrap_or_else(|poison| poison.into_inner())
}

impl ToolRegistry {
    pub fn new(tools: impl IntoIterator<Item = Tool>) -> Result<Self, ToolError> {
        let mut registry = BTreeMap::new();
        for tool in tools {
            if tool.definition.name.is_empty() || registry.contains_key(&tool.definition.name) {
                return Err(ToolError::InvalidArguments(
                    "tool names must be non-empty and unique".to_owned(),
                ));
            }
            registry.insert(tool.definition.name.clone(), tool);
        }
        Ok(Self {
            tools: Arc::new(RwLock::new(registry)),
        })
    }

    pub fn get(&self, name: &str) -> Option<Tool> {
        read_tools(&self.tools).get(name).cloned()
    }

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        read_tools(&self.tools)
            .values()
            .map(|tool| tool.definition.clone())
            .collect()
    }

    /// Rich, deterministic (name-sorted) catalog for the UI tool pickers.
    pub fn catalog(&self) -> Vec<ToolCatalogEntry> {
        let mut entries = read_tools(&self.tools)
            .values()
            .map(|tool| {
                let mut capabilities = tool
                    .capabilities
                    .iter()
                    .map(|capability| capability_name(*capability).to_owned())
                    .collect::<Vec<_>>();
                // Match the Python catalog's `sorted(cap.value ...)`.
                capabilities.sort_unstable();
                ToolCatalogEntry {
                    name: tool.definition.name.clone(),
                    description: tool.definition.description.clone(),
                    parameters: tool.definition.parameters.clone(),
                    capabilities,
                    dangerous: tool.default_decision == Decision::Ask,
                }
            })
            .collect::<Vec<_>>();
        entries.sort_by(|left, right| left.name.cmp(&right.name));
        entries
    }

    /// Returns a new independent registry containing this registry's tools plus
    /// `tools`. The original is untouched (dynamic registrations made later on
    /// either registry are not shared between them).
    pub fn extend(&self, tools: impl IntoIterator<Item = Tool>) -> Result<Self, ToolError> {
        let mut combined = read_tools(&self.tools)
            .values()
            .cloned()
            .collect::<Vec<_>>();
        combined.extend(tools);
        Self::new(combined)
    }

    /// Insert a tool into the shared map at runtime. Fails on an empty name or
    /// a name already present, matching `new`'s uniqueness contract — callers
    /// (e.g. the MCP admin) treat a collision as a skip, never an override.
    pub fn register(&self, tool: Tool) -> Result<(), ToolError> {
        let name = tool.definition.name.clone();
        if name.is_empty() {
            return Err(ToolError::InvalidArguments(
                "tool names must be non-empty and unique".to_owned(),
            ));
        }
        let mut tools = write_tools(&self.tools);
        if tools.contains_key(&name) {
            return Err(ToolError::InvalidArguments(
                "tool names must be non-empty and unique".to_owned(),
            ));
        }
        tools.insert(name, tool);
        Ok(())
    }

    /// Remove a dynamically registered tool; returns whether it was present.
    pub fn unregister(&self, name: &str) -> bool {
        write_tools(&self.tools).remove(name).is_some()
    }
}

pub fn builtin_registry() -> ToolRegistry {
    ToolRegistry::new([
        Tool::new(
            definition("read_file", "Read a UTF-8 workspace file; page with offset_bytes/maxBytes when truncated", json!({"type":"object","properties":{"path":{"type":"string"},"maxBytes":{"type":"integer","minimum":1},"offset_bytes":{"type":"integer","minimum":0}},"required":["path"],"additionalProperties":false})),
            [Capability::Read],
            Decision::Allow,
            ReadFile,
        ),
        Tool::new(
            definition("search_files", "Search workspace file contents with a Rust-syntax regex (gitignore-aware, skips files >4MB and non-UTF-8)", json!({"type":"object","properties":{"pattern":{"type":"string","description":"Regex (Rust regex syntax) to search file contents"},"path":{"type":"string","default":".","description":"Directory or file to search in, relative to workspace"},"glob":{"type":"string","description":"Optional glob filter for file names, e.g. '*.rs' or 'src/**/*.ts'"},"maxResults":{"type":"integer","default":100,"maximum":2000},"context":{"type":"integer","default":0,"description":"Context lines before/after each match"}},"required":["pattern"],"additionalProperties":false})),
            [Capability::Read],
            Decision::Allow,
            SearchFiles,
        ),
        Tool::new(
            definition("find_files", "Find workspace files by glob pattern (gitignore-aware)", json!({"type":"object","properties":{"glob":{"type":"string","description":"Glob pattern, e.g. '**/*.ts'"},"path":{"type":"string","default":"."},"maxResults":{"type":"integer","default":200,"maximum":2000}},"required":["glob"],"additionalProperties":false})),
            [Capability::Read],
            Decision::Allow,
            FindFiles,
        ),
        Tool::new(
            definition("list_files", "List one workspace directory", json!({"type":"object","properties":{"path":{"type":"string"}},"additionalProperties":false})),
            [Capability::Read],
            Decision::Allow,
            ListFiles,
        ),
        Tool::new(
            definition("write_file", "Write a UTF-8 workspace file", json!({"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"},"append":{"type":"boolean"}},"required":["path","content"],"additionalProperties":false})),
            [Capability::Write],
            Decision::Ask,
            WriteFile,
        ),
        Tool::new(
            definition("edit_file", "Apply anchor-based edits to a UTF-8 workspace file: each edit replaces the exact `old` text with `new` (set replace_all for every occurrence). All edits apply atomically — one bad anchor rejects the whole call. With create_if_missing and a single `{\"old\":\"\",\"new\":\"<content>\"}` edit it creates the file. Prefer this over write_file for targeted changes.", json!({"type":"object","properties":{"path":{"type":"string"},"edits":{"type":"array","minItems":1,"items":{"type":"object","properties":{"old":{"type":"string"},"new":{"type":"string"},"replace_all":{"type":"boolean","default":false}},"required":["old","new"],"additionalProperties":false}},"create_if_missing":{"type":"boolean","default":false}},"required":["path","edits"],"additionalProperties":false})),
            [Capability::Write],
            Decision::Ask,
            EditFile,
        ),
        Tool::new(
            definition("shell", "Run an argument-vector process through the configured isolated launcher; fails closed by default. Output over the byte cap is spilled to .cool/spill/ in the workspace (add it to .gitignore) and the result carries head/tail.", process_schema()),
            [Capability::Execute],
            Decision::Ask,
            ProcessTool { git_only: false },
        ),
        Tool::new(
            definition("git", "Run git through the configured isolated launcher; fails closed by default. Output over the byte cap is spilled to .cool/spill/ in the workspace (add it to .gitignore) and the result carries head/tail.", json!({"type":"object","properties":{"args":{"type":"array","items":{"type":"string"}}},"required":["args"],"additionalProperties":false})),
            [Capability::Git, Capability::Execute],
            Decision::Ask,
            ProcessTool { git_only: true },
        ),
        Tool::new(
            definition("update_plan", "Create or update a run plan", json!({"type":"object","properties":{"planId":{"type":"string"},"title":{"type":["string","null"]},"steps":{"type":"array","items":{"type":"object","properties":{"title":{"type":"string"},"status":{"type":"string"}},"required":["title","status"],"additionalProperties":false}}},"required":["planId","steps"],"additionalProperties":false})),
            [],
            Decision::Allow,
            PlanTool,
        ),
    ])
    .expect("builtin tool names are valid")
}

fn definition(name: &str, description: &str, parameters: Value) -> ToolDefinition {
    ToolDefinition {
        name: name.to_owned(),
        description: description.to_owned(),
        parameters,
    }
}

fn process_schema() -> Value {
    json!({"type":"object","properties":{"program":{"type":"string"},"args":{"type":"array","items":{"type":"string"}}},"required":["program","args"],"additionalProperties":false})
}

fn required_string<'a>(arguments: &'a Value, name: &str) -> Result<&'a str, ToolError> {
    arguments
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ToolError::InvalidArguments(format!("{name} must be a non-empty string")))
}

fn required_text<'a>(arguments: &'a Value, name: &str) -> Result<&'a str, ToolError> {
    arguments
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::InvalidArguments(format!("{name} must be a string")))
}

fn reject_unknown(arguments: &Value, allowed: &[&str]) -> Result<(), ToolError> {
    let object = arguments
        .as_object()
        .ok_or_else(|| ToolError::InvalidArguments("arguments must be an object".to_owned()))?;
    if let Some(name) = object.keys().find(|name| !allowed.contains(&name.as_str())) {
        return Err(ToolError::InvalidArguments(format!(
            "unknown argument {name}"
        )));
    }
    Ok(())
}

fn string_array(arguments: &Value, name: &str) -> Result<Vec<String>, ToolError> {
    arguments
        .get(name)
        .and_then(Value::as_array)
        .ok_or_else(|| ToolError::InvalidArguments(format!("{name} must be an array")))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| ToolError::InvalidArguments(format!("{name} must contain strings")))
        })
        .collect()
}

struct ReadFile;

/// Every file-tool open goes through the workspace's cap-std `Dir`, so the
/// handle is resolved component-by-component beneath the root at I/O time and
/// a symlink/reparse swap cannot escape (handle-relative confinement).
fn workspace_path(context: &ToolContext, requested: &str) -> Result<std::path::PathBuf, ToolError> {
    context
        .workspace
        .confine_relative(requested)
        .map_err(|error| ToolError::Security(error.to_string()))
}

fn confinement_io(error: std::io::Error) -> ToolError {
    match error.kind() {
        // cap-std reports sandboxed (escaping) paths as permission failures.
        std::io::ErrorKind::PermissionDenied => ToolError::Security(error.to_string()),
        _ => ToolError::Io(error),
    }
}

#[async_trait]
impl ToolHandler for ReadFile {
    async fn execute(
        &self,
        context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolResult, ToolError> {
        reject_unknown(&arguments, &["path", "maxBytes", "offset_bytes"])?;
        let path = workspace_path(context, required_string(&arguments, "path")?)?;
        let metadata = match context.workspace.dir().metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ToolResult::error("file_not_found", "path is not a file"));
            }
            Err(error) => return Err(confinement_io(error)),
        };
        if !metadata.is_file() {
            return Ok(ToolResult::error("file_not_found", "path is not a file"));
        }
        if arguments
            .get("offset_bytes")
            .is_some_and(|value| value.as_u64().is_none())
        {
            return Err(ToolError::InvalidArguments(
                "offset_bytes must be a non-negative integer".to_owned(),
            ));
        }
        let offset = arguments
            .get("offset_bytes")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let limit = arguments
            .get("maxBytes")
            .and_then(Value::as_u64)
            .unwrap_or(200_000)
            .min(context.max_output_bytes as u64) as usize;
        if limit == 0 {
            return Err(ToolError::InvalidArguments(
                "maxBytes must be positive".to_owned(),
            ));
        }
        let total = metadata.len();
        let bytes = if offset >= total {
            Vec::new()
        } else {
            use std::io::{Read as _, Seek as _, SeekFrom};
            let mut file = context
                .workspace
                .dir()
                .open(&path)
                .map_err(confinement_io)?;
            file.seek(SeekFrom::Start(offset)).map_err(ToolError::Io)?;
            let mut bytes = Vec::new();
            file.take(limit as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(ToolError::Io)?;
            bytes
        };
        let truncated = bytes.len() > limit;
        let text = String::from_utf8_lossy(&bytes[..bytes.len().min(limit)]);
        let mut output = json!({
            "content": mask_secrets(&text),
            "totalBytes": total,
            "truncated": truncated,
        });
        if truncated {
            output["hint"] = json!("use offset_bytes/max_bytes to page");
        }
        Ok(ToolResult {
            output,
            is_error: false,
            error_code: None,
            truncated,
        })
    }
}

struct ListFiles;

#[async_trait]
impl ToolHandler for ListFiles {
    async fn execute(
        &self,
        context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolResult, ToolError> {
        reject_unknown(&arguments, &["path"])?;
        if arguments
            .get("path")
            .is_some_and(|value| !value.is_string())
        {
            return Err(ToolError::InvalidArguments(
                "path must be a string".to_owned(),
            ));
        }
        let requested = arguments.get("path").and_then(Value::as_str).unwrap_or(".");
        let path = workspace_path(context, requested)?;
        let dir = context
            .workspace
            .dir()
            .try_clone()
            .map_err(confinement_io)?;
        // cap-std read_dir is a synchronous iterator — keep it off the
        // async worker.
        let mut entries = tokio::task::spawn_blocking(move || -> Result<Vec<String>, ToolError> {
            let reader = dir.read_dir(&path).map_err(confinement_io)?;
            let mut entries = Vec::new();
            for entry in reader {
                let entry = entry.map_err(confinement_io)?;
                let kind = entry.file_type().map_err(confinement_io)?;
                entries.push(format!(
                    "{}{}",
                    entry.file_name().to_string_lossy(),
                    if kind.is_dir() { "/" } else { "" }
                ));
            }
            Ok(entries)
        })
        .await
        .map_err(|error| ToolError::Io(std::io::Error::other(error)))??;
        entries.sort();
        Ok(ToolResult::ok(json!({"entries": entries})))
    }
}

/// Largest file `search_files` will scan; bigger files are counted and skipped.
const MAX_SEARCH_FILE_BYTES: u64 = 4 * 1024 * 1024;
/// Bound on `context` so a huge request cannot multiply every match's body.
const MAX_SEARCH_CONTEXT_LINES: u64 = 10;

/// Directory inside the workspace receiving over-cap tool output bodies.
const SPILL_DIR: &str = ".cool/spill";
/// Per-file cap so a runaway stream cannot fill the workspace.
const SPILL_FILE_MAX_BYTES: usize = 10 * 1024 * 1024;
const SPILL_HEAD_BYTES: usize = 64 * 1024;
const SPILL_TAIL_BYTES: usize = 16 * 1024;

fn compile_glob(pattern: &str) -> Result<GlobMatcher, ToolError> {
    Glob::new(pattern)
        .map(|glob| glob.compile_matcher())
        .map_err(|error| ToolError::InvalidArguments(format!("invalid glob: {error}")))
}

/// Enumerate regular files beneath `relative` (already confined) without
/// following links, honoring gitignore rules. Returns workspace-relative paths
/// with `/` separators; all reads still go through the capability `Dir`, so a
/// symlink/reparse swap cannot escape the workspace.
fn collect_workspace_files(
    workspace: &Workspace,
    relative: &Path,
    glob: Option<&GlobMatcher>,
) -> Result<Vec<(PathBuf, String, u64)>, ToolError> {
    let root = workspace.root();
    let absolute = root.join(relative);
    if absolute.exists() {
        // Canonicalize + reject_links on every component: a symlinked search
        // root (or ancestor) pointing outside fails closed.
        workspace
            .confine_existing(&absolute)
            .map_err(|error| ToolError::Security(error.to_string()))?;
    }
    let mut files = Vec::new();
    for entry in ignore::WalkBuilder::new(absolute)
        .follow_links(false)
        .build()
        .flatten()
    {
        let Some(kind) = entry.file_type() else {
            continue;
        };
        if !kind.is_file() {
            continue;
        }
        let Ok(rel) = entry.path().strip_prefix(root) else {
            continue;
        };
        let display = rel.to_string_lossy().replace('\\', "/");
        if let Some(matcher) = glob
            && !matcher.is_match(&display)
        {
            continue;
        }
        let size = entry.metadata().map(|meta| meta.len()).unwrap_or(0);
        files.push((rel.to_path_buf(), display, size));
    }
    files.sort_by(|a, b| a.1.cmp(&b.1));
    Ok(files)
}

/// Spill path stem derived from the provider tool-call id when known.
fn spill_stem(context: &ToolContext) -> String {
    let raw = context
        .call_id
        .clone()
        .unwrap_or_else(|| format!("call-{}", Uuid::new_v4().simple()));
    let sanitized: String = raw
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .take(64)
        .collect();
    if sanitized.is_empty() {
        "call".to_owned()
    } else {
        sanitized
    }
}

/// Persist a capped copy of an over-cap body beneath `.cool/spill/` and return
/// its workspace-relative path. The uuid suffix keeps filenames unique when a
/// sanitized call_id repeats across calls or runs.
fn spill_output(
    context: &ToolContext,
    stem: &str,
    label: &str,
    bytes: &[u8],
) -> Result<String, ToolError> {
    let suffix = &Uuid::new_v4().simple().to_string()[..8];
    let relative = Path::new(SPILL_DIR).join(format!("{stem}-{label}-{suffix}.txt"));
    if let Some(parent) = relative.parent() {
        context
            .workspace
            .dir()
            .create_dir_all(parent)
            .map_err(confinement_io)?;
    }
    context
        .workspace
        .dir()
        .write(&relative, &bytes[..bytes.len().min(SPILL_FILE_MAX_BYTES)])
        .map_err(confinement_io)?;
    Ok(relative.to_string_lossy().replace('\\', "/"))
}

fn floor_char_boundary(text: &str, index: usize) -> usize {
    let mut index = index.min(text.len());
    while !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn ceil_char_boundary(text: &str, index: usize) -> usize {
    let mut index = index.min(text.len());
    while !text.is_char_boundary(index) {
        index += 1;
    }
    index
}

/// Bounded head/tail view of `text` pointing at the spilled full body.
fn head_tail_view(text: &str, head: usize, tail: usize, spill_path: &str) -> String {
    if text.len() <= head + tail {
        return text.to_owned();
    }
    let head_end = floor_char_boundary(text, head);
    let tail_start = ceil_char_boundary(text, text.len() - tail);
    let omitted = tail_start.saturating_sub(head_end);
    format!(
        "{}\n... [truncated {} bytes; full output: {}] ...\n{}",
        &text[..head_end],
        omitted,
        spill_path,
        &text[tail_start..]
    )
}

/// Shrink a search/find result until its JSON fits the output cap; the
/// serialized body is spilled to `.cool/spill/` first, trimmed to the spill
/// cap so the file stays complete, parseable JSON (`totalMatches`/`paths`
/// trims still report every hit the collector saw via `truncated`).
fn bound_result(
    context: &ToolContext,
    stem: &str,
    mut output: Value,
) -> Result<(Value, bool), ToolError> {
    let serialize = |value: &Value| -> Result<usize, ToolError> {
        serde_json::to_vec(value)
            .map(|bytes| bytes.len())
            .map_err(|error| ToolError::Io(std::io::Error::other(error)))
    };
    let mut serialized = serialize(&output)?;
    if serialized <= context.max_output_bytes {
        return Ok((output, false));
    }
    output["truncated"] = json!(true);
    // Only read-only tools reach this path; a spill write is a Write-effect,
    // so the policy's Write decision gates it.
    if context.policy.resolve(Capability::Write) == Decision::Allow {
        for key in ["matches", "paths"] {
            if output.get(key).and_then(Value::as_array).is_none() {
                continue;
            }
            while serialized > SPILL_FILE_MAX_BYTES
                && output[key].as_array().is_some_and(|list| !list.is_empty())
            {
                let list = output[key].as_array_mut().unwrap();
                let drop = (serialized - SPILL_FILE_MAX_BYTES)
                    / (serialized / list.len().max(1)).max(1)
                    + 1;
                list.truncate(list.len().saturating_sub(drop));
                serialized = serialize(&output)?;
            }
            break;
        }
        let bytes = serde_json::to_vec(&output)
            .map_err(|error| ToolError::Io(std::io::Error::other(error)))?;
        let spill_path = spill_output(context, stem, "results", &bytes)?;
        output["spillPath"] = json!(spill_path);
    } else {
        output["spillSkipped"] = json!("Write capability denied by policy");
    }
    for key in ["matches", "paths"] {
        if output.get(key).and_then(Value::as_array).is_none() {
            continue;
        }
        let mut serialized = serialize(&output)?;
        while serialized > context.max_output_bytes
            && output[key].as_array().is_some_and(|list| !list.is_empty())
        {
            let list = output[key].as_array_mut().unwrap();
            let drop = (serialized - context.max_output_bytes)
                / (serialized / list.len().max(1)).max(1)
                + 1;
            list.truncate(list.len().saturating_sub(drop));
            serialized = serialize(&output)?;
        }
        break;
    }
    Ok((output, true))
}

struct SearchFiles;

#[async_trait]
impl ToolHandler for SearchFiles {
    async fn execute(
        &self,
        context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolResult, ToolError> {
        reject_unknown(
            &arguments,
            &["pattern", "path", "glob", "maxResults", "context"],
        )?;
        for name in ["path", "glob"] {
            if arguments.get(name).is_some_and(|value| !value.is_string()) {
                return Err(ToolError::InvalidArguments(format!(
                    "{name} must be a string"
                )));
            }
        }
        for name in ["maxResults", "context"] {
            if arguments.get(name).is_some_and(|value| !value.is_u64()) {
                return Err(ToolError::InvalidArguments(format!(
                    "{name} must be a non-negative integer"
                )));
            }
        }
        let regex = Regex::new(required_string(&arguments, "pattern")?)
            .map_err(|error| ToolError::InvalidArguments(format!("invalid pattern: {error}")))?;
        let glob = arguments
            .get("glob")
            .and_then(Value::as_str)
            .map(compile_glob)
            .transpose()?;
        let max_results = arguments
            .get("maxResults")
            .and_then(Value::as_u64)
            .unwrap_or(100)
            .min(2000) as usize;
        let context_lines = arguments
            .get("context")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .min(MAX_SEARCH_CONTEXT_LINES) as usize;
        let relative = workspace_path(
            context,
            arguments.get("path").and_then(Value::as_str).unwrap_or("."),
        )?;
        let workspace = context.workspace.clone();
        let stem = spill_stem(context);
        let output = tokio::task::spawn_blocking(move || -> Result<Value, ToolError> {
            let mut matches = Vec::new();
            let mut total = 0_u64;
            let mut skipped_large = 0_u64;
            let mut stored_bytes = 0_usize;
            for (rel, display, size) in
                collect_workspace_files(&workspace, &relative, glob.as_ref())?
            {
                if size > MAX_SEARCH_FILE_BYTES {
                    skipped_large += 1;
                    continue;
                }
                let Ok(bytes) = workspace.dir().read(&rel) else {
                    continue;
                };
                let Ok(text) = std::str::from_utf8(&bytes) else {
                    continue;
                };
                let lines: Vec<&str> = text.lines().collect();
                for (index, line) in lines.iter().enumerate() {
                    for hit in regex.find_iter(line) {
                        total += 1;
                        // Keep counting every match but stop storing once the
                        // result is past the spill bound — the spilled body
                        // and its head/tail view are built from what was
                        // retained.
                        if matches.len() >= max_results || stored_bytes > SPILL_FILE_MAX_BYTES {
                            continue;
                        }
                        let column = line[..hit.start()].chars().count() + 1;
                        let mut context_lines_json = Vec::new();
                        let before = index.saturating_sub(context_lines);
                        for (offset, text_line) in lines[before..index].iter().enumerate() {
                            context_lines_json.push(json!({
                                "line": before + offset + 1,
                                "text": mask_secrets(text_line),
                            }));
                        }
                        let after_end = (index + 1 + context_lines).min(lines.len());
                        for (offset, text_line) in lines[index + 1..after_end].iter().enumerate() {
                            context_lines_json.push(json!({
                                "line": index + 2 + offset,
                                "text": mask_secrets(text_line),
                            }));
                        }
                        stored_bytes += line.len()
                            + context_lines_json
                                .iter()
                                .map(|entry| entry["text"].as_str().map_or(0, str::len))
                                .sum::<usize>();
                        matches.push(json!({
                            "path": display,
                            "line": index + 1,
                            "column": column,
                            "text": mask_secrets(line),
                            "context": context_lines_json,
                        }));
                    }
                }
            }
            let truncated = total as usize > matches.len();
            Ok(json!({
                "matches": matches,
                "totalMatches": total,
                "truncated": truncated,
                "skippedLargeFiles": skipped_large,
            }))
        })
        .await
        .map_err(|error| ToolError::Io(std::io::Error::other(error)))??;
        let (output, spilled) = bound_result(context, &stem, output)?;
        let truncated = spilled || output["truncated"].as_bool().unwrap_or(false);
        Ok(ToolResult {
            output,
            is_error: false,
            error_code: None,
            truncated,
        })
    }
}

struct FindFiles;

#[async_trait]
impl ToolHandler for FindFiles {
    async fn execute(
        &self,
        context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolResult, ToolError> {
        reject_unknown(&arguments, &["glob", "path", "maxResults"])?;
        if arguments
            .get("path")
            .is_some_and(|value| !value.is_string())
        {
            return Err(ToolError::InvalidArguments(
                "path must be a string".to_owned(),
            ));
        }
        if arguments
            .get("maxResults")
            .is_some_and(|value| !value.is_u64())
        {
            return Err(ToolError::InvalidArguments(
                "maxResults must be a non-negative integer".to_owned(),
            ));
        }
        let glob = compile_glob(required_string(&arguments, "glob")?)?;
        let max_results = arguments
            .get("maxResults")
            .and_then(Value::as_u64)
            .unwrap_or(200)
            .min(2000) as usize;
        let relative = workspace_path(
            context,
            arguments.get("path").and_then(Value::as_str).unwrap_or("."),
        )?;
        let workspace = context.workspace.clone();
        let stem = spill_stem(context);
        let output = tokio::task::spawn_blocking(move || -> Result<Value, ToolError> {
            let paths: Vec<String> = collect_workspace_files(&workspace, &relative, Some(&glob))?
                .into_iter()
                .take(max_results + 1)
                .map(|(_, display, _)| display)
                .collect();
            Ok(json!({
                "paths": paths.iter().take(max_results).cloned().collect::<Vec<_>>(),
                "truncated": paths.len() > max_results,
            }))
        })
        .await
        .map_err(|error| ToolError::Io(std::io::Error::other(error)))??;
        let (output, spilled) = bound_result(context, &stem, output)?;
        let truncated = spilled || output["truncated"].as_bool().unwrap_or(false);
        Ok(ToolResult {
            output,
            is_error: false,
            error_code: None,
            truncated,
        })
    }
}

struct WriteFile;

#[async_trait]
impl ToolHandler for WriteFile {
    async fn execute(
        &self,
        context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolResult, ToolError> {
        reject_unknown(&arguments, &["path", "content", "append"])?;
        let requested = required_string(&arguments, "path")?;
        let content = required_text(&arguments, "content")?;
        if arguments
            .get("append")
            .is_some_and(|value| !value.is_boolean())
        {
            return Err(ToolError::InvalidArguments(
                "append must be a boolean".to_owned(),
            ));
        }
        let append = arguments
            .get("append")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let path = workspace_path(context, requested)?;
        if let Some(parent) = path.parent()
            && parent.components().next().is_some()
        {
            context
                .workspace
                .dir()
                .create_dir_all(parent)
                .map_err(confinement_io)?;
        }
        let mut options = cap_std::fs::OpenOptions::new();
        options.write(true).create(true);
        if append {
            options.append(true);
        } else {
            options.truncate(true);
        }
        use std::io::Write as _;
        let mut file = context
            .workspace
            .dir()
            .open_with(&path, &options)
            .map_err(confinement_io)?;
        file.write_all(content.as_bytes()).map_err(ToolError::Io)?;
        file.flush().map_err(ToolError::Io)?;
        Ok(ToolResult::ok(json!({
            "path": requested,
            "bytes": content.len(),
            "append": append,
        })))
    }
}

struct EditFile;

/// One `{old,new,replace_all}` anchor edit as validated from the arguments.
struct AnchorEdit {
    old: String,
    new: String,
    replace_all: bool,
}

fn parse_edits(arguments: &Value) -> Result<Vec<AnchorEdit>, ToolError> {
    let edits = arguments
        .get("edits")
        .and_then(Value::as_array)
        .filter(|edits| !edits.is_empty())
        .ok_or_else(|| ToolError::InvalidArguments("edits must be a non-empty array".to_owned()))?;
    edits
        .iter()
        .enumerate()
        .map(|(index, edit)| {
            let object = edit.as_object().ok_or_else(|| {
                ToolError::InvalidArguments(format!("edits[{index}] must be an object"))
            })?;
            if let Some(key) = object
                .keys()
                .find(|key| !matches!(key.as_str(), "old" | "new" | "replace_all"))
            {
                return Err(ToolError::InvalidArguments(format!(
                    "edits[{index}]: unknown field {key}"
                )));
            }
            let old = object.get("old").and_then(Value::as_str).ok_or_else(|| {
                ToolError::InvalidArguments(format!("edits[{index}].old must be a string"))
            })?;
            let new = object.get("new").and_then(Value::as_str).ok_or_else(|| {
                ToolError::InvalidArguments(format!("edits[{index}].new must be a string"))
            })?;
            let replace_all = match object.get("replace_all") {
                Some(value) => value.as_bool().ok_or_else(|| {
                    ToolError::InvalidArguments(format!(
                        "edits[{index}].replace_all must be a boolean"
                    ))
                })?,
                None => false,
            };
            Ok(AnchorEdit {
                old: old.to_owned(),
                new: new.to_owned(),
                replace_all,
            })
        })
        .collect()
}

fn edit_error(code: &str, message: impl Into<String>, extra: Value) -> ToolResult {
    let mut output = extra;
    output["error"] = json!(message.into());
    output["errorCode"] = json!(code);
    ToolResult {
        output,
        is_error: true,
        error_code: Some(code.to_owned()),
        truncated: false,
    }
}

#[async_trait]
impl ToolHandler for EditFile {
    async fn execute(
        &self,
        context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolResult, ToolError> {
        reject_unknown(&arguments, &["path", "edits", "create_if_missing"])?;
        let requested = required_string(&arguments, "path")?;
        let create_if_missing = match arguments.get("create_if_missing") {
            Some(value) => value.as_bool().ok_or_else(|| {
                ToolError::InvalidArguments("create_if_missing must be a boolean".to_owned())
            })?,
            None => false,
        };
        let edits = parse_edits(&arguments)?;
        let mut path = workspace_path(context, requested)?;

        // Follow a symlink to its in-workspace target: writing through a
        // temp file + rename would replace the link itself and leave the
        // linked file untouched. `read_link` rejects absolute targets.
        // Chained links resolve fully, capped at 8 hops.
        for _ in 0..8 {
            let Ok(link_metadata) = context.workspace.dir().symlink_metadata(&path) else {
                break;
            };
            if !link_metadata.file_type().is_symlink() {
                break;
            }
            let target = context
                .workspace
                .dir()
                .read_link(&path)
                .map_err(confinement_io)?;
            let resolved = match path.parent() {
                Some(parent) => parent.join(target),
                None => target,
            };
            path = workspace_path(
                context,
                resolved.to_str().ok_or_else(|| {
                    ToolError::InvalidArguments("symlink target is not a valid path".to_owned())
                })?,
            )?;
        }
        if context
            .workspace
            .dir()
            .symlink_metadata(&path)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
        {
            return Err(ToolError::InvalidArguments(
                "symlink chain exceeds 8 hops".to_owned(),
            ));
        }

        let mut original_permissions = None;
        let before = match context.workspace.dir().metadata(&path) {
            Ok(metadata) if metadata.is_file() => {
                original_permissions = Some(metadata.permissions());
                let mut text = String::new();
                use std::io::Read as _;
                context
                    .workspace
                    .dir()
                    .open(&path)
                    .map_err(confinement_io)?
                    .read_to_string(&mut text)
                    .map_err(ToolError::Io)?;
                Some(text)
            }
            Ok(_) => return Ok(ToolResult::error("file_not_found", "path is not a file")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(confinement_io(error)),
        };

        let mut created = false;
        let after = match &before {
            None => {
                if !create_if_missing {
                    return Ok(ToolResult::error("file_not_found", "path is not a file"));
                }
                if edits.len() != 1 || !edits[0].old.is_empty() {
                    return Ok(edit_error(
                        "edit_invalid_create",
                        "creating a missing file requires exactly one edit with an empty old",
                        json!({"edits": edits.len()}),
                    ));
                }
                created = true;
                edits[0].new.clone()
            }
            Some(before) => {
                let mut current = before.clone();
                for (index, edit) in edits.iter().enumerate() {
                    if edit.old.is_empty() {
                        return Ok(edit_error(
                            "edit_empty_anchor",
                            format!("edits[{index}].old must be non-empty for an existing file"),
                            json!({"editIndex": index}),
                        ));
                    }
                    let occurrences = current.matches(&edit.old).count();
                    if occurrences == 0 {
                        return Ok(edit_error(
                            "edit_not_found",
                            format!("edits[{index}].old not found in {requested}"),
                            json!({"editIndex": index}),
                        ));
                    }
                    if occurrences > 1 && !edit.replace_all {
                        return Ok(edit_error(
                            "edit_not_unique",
                            format!(
                                "edits[{index}].old matches {occurrences} locations in {requested}; pass replace_all to change all"
                            ),
                            json!({"editIndex": index, "occurrences": occurrences}),
                        ));
                    }
                    current = if edit.replace_all {
                        current.replace(&edit.old, &edit.new)
                    } else {
                        current.replacen(&edit.old, &edit.new, 1)
                    };
                }
                current
            }
        };

        if let Some(parent) = path.parent()
            && parent.components().next().is_some()
        {
            context
                .workspace
                .dir()
                .create_dir_all(parent)
                .map_err(confinement_io)?;
        }
        // Write the new content to a sibling temp file and rename it over the
        // target so a write failure cannot leave a truncated original.
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| ToolError::InvalidArguments("path must name a file".to_owned()))?;
        let tmp_path = path.with_file_name(format!(".{file_name}.cool-edit-{}", Uuid::new_v4()));
        let mut options = cap_std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        use std::io::Write as _;
        let write_result = (|| {
            let mut file = context
                .workspace
                .dir()
                .open_with(&tmp_path, &options)
                .map_err(confinement_io)?;
            // The temp file's fresh inode carries default permissions —
            // apply the original mode before content is written so a private
            // file never sits world-readable on disk.
            if let Some(permissions) = original_permissions {
                file.set_permissions(permissions).map_err(ToolError::Io)?;
            }
            file.write_all(after.as_bytes()).map_err(ToolError::Io)?;
            file.flush().map_err(ToolError::Io)?;
            context
                .workspace
                .dir()
                .rename(&tmp_path, context.workspace.dir(), &path)
                .map_err(confinement_io)
        })();
        if let Err(error) = write_result {
            let _ = context.workspace.dir().remove_file(&tmp_path);
            return Err(error);
        }

        const DIFF_LIMIT: usize = 4096;
        let (diff, diff_truncated) = unified_diff(
            requested,
            before.as_deref().unwrap_or(""),
            &after,
            DIFF_LIMIT,
        );
        let mut output = json!({
            "path": requested,
            "editsApplied": edits.len(),
            "bytesBefore": before.as_deref().unwrap_or("").len(),
            "bytesAfter": after.len(),
            "diff": mask_secrets(&diff),
        });
        if created {
            output["created"] = json!(true);
        }
        if diff_truncated {
            output["diffTruncated"] = json!(true);
        }
        Ok(ToolResult::ok(output))
    }
}

/// Myers O(ND) line diff producing one op byte per input line (` ` keep,
/// `-` delete, `+` insert). Returns None when the edit distance is too
/// large to trace within a sane memory bound — the caller then emits a
/// single coarse hunk instead.
fn line_ops(a: &[&str], b: &[&str]) -> Option<Vec<u8>> {
    const MAX_D: usize = 1024;
    let (n, m) = (a.len() as i64, b.len() as i64);
    if n + m > 20_000 {
        return None;
    }
    let off = n + m;
    let mut v = vec![0i64; (2 * (n + m) + 1) as usize];
    // trace[d][(k + d) / 2] = furthest x on diagonal k after step d.
    let mut trace: Vec<Vec<i64>> = Vec::new();
    let mut reached = false;
    'outer: for d in 0..=MAX_D as i64 {
        let mut vd = vec![0i64; d as usize + 1];
        let mut k = -d;
        while k <= d {
            let ki = (k + off) as usize;
            let mut x = if k == -d || (k != d && v[ki - 1] < v[ki + 1]) {
                v[ki + 1]
            } else {
                v[ki - 1] + 1
            };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[ki] = x;
            vd[((k + d) / 2) as usize] = x;
            if x >= n && y >= m {
                reached = true;
                break 'outer;
            }
            k += 2;
        }
        trace.push(vd);
    }
    if !reached {
        return None;
    }
    let mut ops = Vec::with_capacity((n + m) as usize);
    let (mut x, mut y) = (n, m);
    for d in (1..=trace.len() as i64).rev() {
        let k = x - y;
        let prev = &trace[(d - 1) as usize];
        let index = |kk: i64| ((kk + d - 1) / 2) as usize;
        let prev_k = if k == -d || (k != d && prev[index(k - 1)] < prev[index(k + 1)]) {
            k + 1
        } else {
            k - 1
        };
        let prev_x = prev[index(prev_k)];
        let prev_y = prev_x - prev_k;
        while x > prev_x && y > prev_y {
            ops.push(b' ');
            x -= 1;
            y -= 1;
        }
        if x == prev_x {
            ops.push(b'+');
            y -= 1;
        } else {
            ops.push(b'-');
            x -= 1;
        }
    }
    while x > 0 && y > 0 {
        ops.push(b' ');
        x -= 1;
        y -= 1;
    }
    ops.reverse();
    Some(ops)
}

/// ~60-line unified diff renderer: `--- a/ +++ b/` headers and `@@` hunks
/// with 3 context lines, neighbouring hunks merged when their context
/// windows touch.
fn unified_diff(path: &str, before: &str, after: &str, max_len: usize) -> (String, bool) {
    const CONTEXT: usize = 3;
    if before == after {
        return (String::new(), false);
    }
    // Emission stops once the output passes `max_len` — large-file edits
    // cannot allocate an unbounded diff string.
    fn finish(mut out: String, truncated: bool) -> (String, bool) {
        if truncated {
            out.push_str("… diff truncated\n");
        }
        (out, truncated)
    }
    let a: Vec<&str> = if before.is_empty() {
        Vec::new()
    } else {
        before.split('\n').collect()
    };
    let b: Vec<&str> = if after.is_empty() {
        Vec::new()
    } else {
        after.split('\n').collect()
    };
    let mut out = format!("--- a/{path}\n+++ b/{path}\n");
    let Some(ops) = line_ops(&a, &b) else {
        out.push_str(&format!("@@ -1,{} +1,{} @@\n", a.len(), b.len()));
        for line in &a {
            out.push('-');
            out.push_str(line);
            out.push('\n');
            if out.len() > max_len {
                return finish(out, true);
            }
        }
        for line in &b {
            out.push('+');
            out.push_str(line);
            out.push('\n');
            if out.len() > max_len {
                return finish(out, true);
            }
        }
        return finish(out, false);
    };
    let mut entries: Vec<(u8, &str)> = Vec::with_capacity(ops.len());
    let (mut i, mut j) = (0usize, 0usize);
    for op in &ops {
        match op {
            b' ' => {
                entries.push((b' ', a[i]));
                i += 1;
                j += 1;
            }
            b'-' => {
                entries.push((b'-', a[i]));
                i += 1;
            }
            _ => {
                entries.push((b'+', b[j]));
                j += 1;
            }
        }
    }
    // a/b line index each entry starts at (0-based).
    let mut positions: Vec<(usize, usize)> = Vec::with_capacity(entries.len());
    let (mut ai, mut bi) = (0usize, 0usize);
    for (op, _) in &entries {
        positions.push((ai, bi));
        match op {
            b' ' => {
                ai += 1;
                bi += 1;
            }
            b'-' => ai += 1,
            _ => bi += 1,
        }
    }
    let mut hunks: Vec<(usize, usize)> = Vec::new();
    for (index, (op, _)) in entries.iter().enumerate() {
        if *op == b' ' {
            continue;
        }
        let lo = index.saturating_sub(CONTEXT);
        let hi = (index + CONTEXT + 1).min(entries.len());
        match hunks.last_mut() {
            Some((_, last_hi)) if lo <= *last_hi => *last_hi = (*last_hi).max(hi),
            _ => hunks.push((lo, hi)),
        }
    }
    for (lo, hi) in hunks {
        let mut old_len = 0usize;
        let mut new_len = 0usize;
        for (op, _) in &entries[lo..hi] {
            match op {
                b' ' => {
                    old_len += 1;
                    new_len += 1;
                }
                b'-' => old_len += 1,
                _ => new_len += 1,
            }
        }
        let old_start = if old_len == 0 {
            positions[lo].0
        } else {
            positions[lo].0 + 1
        };
        let new_start = if new_len == 0 {
            positions[lo].1
        } else {
            positions[lo].1 + 1
        };
        out.push_str(&format!(
            "@@ -{old_start},{old_len} +{new_start},{new_len} @@\n"
        ));
        for (op, text) in &entries[lo..hi] {
            out.push(*op as char);
            out.push_str(text);
            out.push('\n');
            if out.len() > max_len {
                return finish(out, true);
            }
        }
    }
    finish(out, false)
}

struct ProcessTool {
    git_only: bool,
}

#[async_trait]
impl ToolHandler for ProcessTool {
    async fn execute(
        &self,
        context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolResult, ToolError> {
        if self.git_only {
            reject_unknown(&arguments, &["args"])?;
        } else {
            reject_unknown(&arguments, &["program", "args"])?;
        }
        let (program, args) = if self.git_only {
            (PathBuf::from("git"), string_array(&arguments, "args")?)
        } else {
            (
                PathBuf::from(required_string(&arguments, "program")?),
                string_array(&arguments, "args")?,
            )
        };
        run_bounded_process(context, &program, &args, None).await
    }
}

struct PlanTool;

#[async_trait]
impl ToolHandler for PlanTool {
    async fn execute(
        &self,
        _context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolResult, ToolError> {
        reject_unknown(&arguments, &["planId", "title", "steps"])?;
        let plan_id = required_string(&arguments, "planId")?;
        let steps = arguments
            .get("steps")
            .and_then(Value::as_array)
            .ok_or_else(|| ToolError::InvalidArguments("steps must be an array".to_owned()))?;
        Ok(ToolResult::ok(json!({
            "planId": plan_id,
            "title": arguments.get("title").cloned().unwrap_or(Value::Null),
            "steps": steps,
        })))
    }
}

#[derive(Clone)]
pub struct PythonFallbackTool {
    executable: PathBuf,
    script: PathBuf,
    name: String,
}

impl PythonFallbackTool {
    pub fn new(name: impl Into<String>, executable: PathBuf, script: PathBuf) -> Self {
        Self {
            executable,
            script,
            name: name.into(),
        }
    }

    pub fn registration(self) -> Tool {
        let name = self.name.clone();
        Tool::new(
            definition(
                &name,
                "Compatibility fallback implemented by an isolated Python process",
                json!({"type":"object"}),
            ),
            [Capability::Execute],
            Decision::Ask,
            self,
        )
    }
}

#[async_trait]
impl ToolHandler for PythonFallbackTool {
    async fn execute(
        &self,
        context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolResult, ToolError> {
        run_bounded_process(
            context,
            &self.executable,
            &[self.script.to_string_lossy().into_owned()],
            Some(
                serde_json::to_vec(&json!({"tool": self.name, "arguments": arguments}))
                    .map_err(|error| ToolError::InvalidArguments(error.to_string()))?,
            ),
        )
        .await
    }
}

async fn run_bounded_process(
    context: &ToolContext,
    program: &Path,
    args: &[String],
    stdin: Option<Vec<u8>>,
) -> Result<ToolResult, ToolError> {
    if !context.allow_trusted_host_processes {
        return Err(ToolError::Security(
            "OS-isolated process launcher is not configured; trusted-host execution is disabled"
                .to_owned(),
        ));
    }
    let safe_environment = sanitize_environment(
        context
            .environment
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str())),
        &BTreeSet::new(),
    );
    let secret_values = context
        .environment
        .iter()
        .filter(|(name, _)| !safe_environment.contains_key(*name))
        .map(|(_, value)| value.clone())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    let environment = sanitize_environment(
        context
            .environment
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str())),
        &context.allowed_secret_environment,
    );
    let mut command = process_wrap::tokio::CommandWrap::with_new(program, |command| {
        command
            .args(args)
            .current_dir(context.workspace.root())
            .env_clear()
            .envs(environment)
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
    });
    // OS-isolated launcher: the child and any descendants live in a killable
    // containment unit — a Windows Job Object (closed on drop/kill) or a Unix
    // process group (kill hits the whole group, not just the direct child).
    command.wrap(process_wrap::tokio::KillOnDrop);
    #[cfg(unix)]
    command.wrap(process_wrap::tokio::ProcessGroup::leader());
    #[cfg(windows)]
    command.wrap(process_wrap::tokio::JobObject);
    let mut child = command.spawn().map_err(ToolError::Io)?;
    if let Some(stdin) = stdin
        && let Some(mut pipe) = child.stdin().take()
    {
        use tokio::io::AsyncWriteExt as _;
        pipe.write_all(&stdin).await?;
    }
    let stdout = child
        .stdout()
        .take()
        .ok_or_else(|| ToolError::Io(std::io::Error::other("missing stdout")))?;
    let stderr = child
        .stderr()
        .take()
        .ok_or_else(|| ToolError::Io(std::io::Error::other("missing stderr")))?;
    // Drain each stream fully so the child never blocks on a full pipe,
    // retaining head + true tail within the spill-file cap.
    let head_cap = SPILL_FILE_MAX_BYTES - SPILL_TAIL_BYTES - 256;
    let stdout_task =
        tokio::spawn(async move { drain_stream(stdout, head_cap, SPILL_TAIL_BYTES).await });
    let stderr_task =
        tokio::spawn(async move { drain_stream(stderr, head_cap, SPILL_TAIL_BYTES).await });
    let status = if let Some(mut cancel) = context.cancel.clone() {
        tokio::select! {
            waited = timeout(context.timeout, child.wait()) => match waited {
                Ok(status) => status?,
                Err(_) => {
                    let _ = std::pin::Pin::from(child.kill()).await;
                    let _ = child.wait().await;
                    return Err(ToolError::Timeout);
                }
            },
            _ = cancel.wait() => {
                let _ = std::pin::Pin::from(child.kill()).await;
                let _ = child.wait().await;
                return Err(ToolError::Cancelled);
            }
        }
    } else {
        match timeout(context.timeout, child.wait()).await {
            Ok(status) => status?,
            Err(_) => {
                let _ = std::pin::Pin::from(child.kill()).await;
                let _ = child.wait().await;
                return Err(ToolError::Timeout);
            }
        }
    };
    let stdout = stdout_task
        .await
        .map_err(|error| ToolError::Io(std::io::Error::other(error)))??;
    let stderr = stderr_task
        .await
        .map_err(|error| ToolError::Io(std::io::Error::other(error)))??;
    // Whether the stream itself was cut at the capture cap — independent of
    // the output budget, so `truncated` stays honest at larger budgets.
    let stdout_cut = stdout.truncated();
    let stderr_cut = stderr.truncated();
    let mut stdout = String::from_utf8_lossy(&stdout.body()).into_owned();
    let mut stderr = String::from_utf8_lossy(&stderr.body()).into_owned();
    for secret in secret_values {
        stdout = stdout.replace(&secret, "[REDACTED]");
        stderr = stderr.replace(&secret, "[REDACTED]");
    }
    let stdout = mask_secrets(&stdout);
    let stderr = mask_secrets(&stderr);
    let stem = spill_stem(context);
    let mut output = json!({
        "exitCode": status.code(),
        "success": status.success(),
        "truncated": false,
    });
    let mut truncated = false;
    // stdout and stderr share the output budget — each stream spills to
    // `.cool/spill/` when it would push the combined body over the cap. A
    // stream already cut at the capture cap reports truncated even when the
    // head/marker/tail body fits in-band.
    let mut budget = context.max_output_bytes;
    for (stream, body, cut) in [
        ("stdout", &stdout, stdout_cut),
        ("stderr", &stderr, stderr_cut),
    ] {
        if body.len() <= budget {
            budget -= body.len();
            output[stream] = json!(body);
            truncated |= cut;
            continue;
        }
        let spill_path = spill_output(context, &stem, stream, body.as_bytes())?;
        // Reserve room for the truncation marker itself.
        let inner = budget.saturating_sub(160);
        if inner == 0 {
            output[stream] = json!("");
        } else {
            let tail = SPILL_TAIL_BYTES.min(inner / 4);
            let head = SPILL_HEAD_BYTES.min(inner.saturating_sub(tail));
            let view = head_tail_view(body, head, tail, &spill_path);
            budget = budget.saturating_sub(view.len());
            output[stream] = json!(view);
        }
        budget = budget.saturating_sub(spill_path.len());
        output[format!("{stream}SpillPath")] = json!(spill_path);
        truncated = true;
    }
    output["truncated"] = json!(truncated);
    Ok(ToolResult {
        output,
        is_error: !status.success(),
        error_code: (!status.success()).then(|| "process_failed".to_owned()),
        truncated,
    })
}

/// Bounded capture of one process stream: the leading bytes plus a ring of the
/// trailing bytes, so the spill file and returned tail keep the command's real
/// ending even when the stream overflows the spill cap.
struct StreamCapture {
    head: Vec<u8>,
    tail: Vec<u8>,
    total: u64,
}

impl StreamCapture {
    fn truncated(&self) -> bool {
        self.head.len() as u64 != self.total
    }

    /// Reassemble the retained body: head + omission marker + true tail.
    fn body(&self) -> Vec<u8> {
        if !self.truncated() {
            return self.head.clone();
        }
        let covered = self.head.len() as u64;
        let tail_kept = self.tail.len() as u64;
        let beyond = (self.total - covered).min(tail_kept);
        let omitted = self.total - covered - beyond;
        let mut body = self.head.clone();
        if omitted > 0 {
            body.extend_from_slice(
                format!("\n... [{omitted} bytes omitted mid-stream] ...\n").as_bytes(),
            );
        }
        body.extend_from_slice(&self.tail[self.tail.len() - beyond as usize..]);
        body
    }
}

/// Drain `reader` to EOF so the child never blocks on a full pipe, retaining
/// the leading `head_cap` bytes and the trailing `tail_cap` bytes.
async fn drain_stream<R>(
    mut reader: R,
    head_cap: usize,
    tail_cap: usize,
) -> std::io::Result<StreamCapture>
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt as _;
    let mut head = Vec::new();
    let mut tail = Vec::new();
    let mut total = 0_u64;
    let mut chunk = [0_u8; 8192];
    loop {
        let read = reader.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        total += read as u64;
        let room = head_cap.saturating_sub(head.len());
        if room > 0 {
            head.extend_from_slice(&chunk[..read.min(room)]);
        }
        tail.extend_from_slice(&chunk[..read]);
        let excess = tail.len().saturating_sub(tail_cap);
        if excess > 0 {
            tail.drain(..excess);
        }
    }
    Ok(StreamCapture { head, tail, total })
}
