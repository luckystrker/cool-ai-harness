use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use cool_security::{
    Capability, CapabilityPolicy, Decision, Workspace, mask_json, mask_secrets,
    sanitize_environment,
};
use serde_json::{Value, json};
use tokio::io::AsyncReadExt as _;
use tokio::process::Command;
use tokio::time::timeout;

use crate::loop_runtime::CancelSignal;

#[derive(Clone, Debug)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(Clone, Debug)]
pub struct ToolResult {
    pub output: Value,
    pub is_error: bool,
    pub error_code: Option<String>,
}

impl ToolResult {
    pub fn ok(output: Value) -> Self {
        Self {
            output,
            is_error: false,
            error_code: None,
        }
    }

    pub fn error(code: impl Into<String>, message: impl Into<String>) -> Self {
        let code = code.into();
        Self {
            output: json!({"error": message.into()}),
            is_error: true,
            error_code: Some(code),
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
    OutputTooLarge,
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
            Self::OutputTooLarge => formatter.write_str("tool output exceeded configured limit"),
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
        }
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

#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: Arc<BTreeMap<String, Tool>>,
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
            tools: Arc::new(registry),
        })
    }

    pub fn get(&self, name: &str) -> Option<Tool> {
        self.tools.get(name).cloned()
    }

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools
            .values()
            .map(|tool| tool.definition.clone())
            .collect()
    }
}

pub fn builtin_registry() -> ToolRegistry {
    ToolRegistry::new([
        Tool::new(
            definition("read_file", "Read a UTF-8 workspace file", json!({"type":"object","properties":{"path":{"type":"string"},"maxBytes":{"type":"integer","minimum":1}},"required":["path"],"additionalProperties":false})),
            [Capability::Read],
            Decision::Allow,
            ReadFile,
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
            definition("shell", "Run an argument-vector process through the configured isolated launcher; fails closed by default", process_schema()),
            [Capability::Execute],
            Decision::Ask,
            ProcessTool { git_only: false },
        ),
        Tool::new(
            definition("git", "Run git through the configured isolated launcher; fails closed by default", json!({"type":"object","properties":{"args":{"type":"array","items":{"type":"string"}}},"required":["args"],"additionalProperties":false})),
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

#[async_trait]
impl ToolHandler for ReadFile {
    async fn execute(
        &self,
        context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolResult, ToolError> {
        reject_unknown(&arguments, &["path", "maxBytes"])?;
        let path = context
            .workspace
            .confine_existing(required_string(&arguments, "path")?)
            .map_err(|error| ToolError::Security(error.to_string()))?;
        if !path.is_file() {
            return Ok(ToolResult::error("file_not_found", "path is not a file"));
        }
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
        let bytes = tokio::fs::read(&path).await?;
        let truncated = bytes.len() > limit;
        let text = String::from_utf8_lossy(&bytes[..bytes.len().min(limit)]);
        Ok(ToolResult::ok(json!({
            "content": mask_secrets(&text),
            "bytes": bytes.len(),
            "truncated": truncated,
        })))
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
        let path = context
            .workspace
            .confine_existing(requested)
            .map_err(|error| ToolError::Security(error.to_string()))?;
        let mut reader = tokio::fs::read_dir(path).await?;
        let mut entries = Vec::new();
        while let Some(entry) = reader.next_entry().await? {
            let kind = entry.file_type().await?;
            entries.push(format!(
                "{}{}",
                entry.file_name().to_string_lossy(),
                if kind.is_dir() { "/" } else { "" }
            ));
        }
        entries.sort();
        Ok(ToolResult::ok(json!({"entries": entries})))
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
        let path = if context.workspace.root().join(requested).exists() {
            context.workspace.confine_existing(requested)
        } else {
            context.workspace.confine_for_create(requested)
        }
        .map_err(|error| ToolError::Security(error.to_string()))?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
            context
                .workspace
                .confine_existing(parent)
                .map_err(|error| ToolError::Security(error.to_string()))?;
        }
        if path.exists() {
            context
                .workspace
                .confine_existing(&path)
                .map_err(|error| ToolError::Security(error.to_string()))?;
        }
        let mut options = tokio::fs::OpenOptions::new();
        options.write(true).create(true);
        if append {
            options.append(true);
        } else {
            options.truncate(true);
        }
        use tokio::io::AsyncWriteExt as _;
        let mut file = options.open(&path).await?;
        file.write_all(content.as_bytes()).await?;
        file.flush().await?;
        Ok(ToolResult::ok(json!({
            "path": requested,
            "bytes": content.len(),
            "append": append,
        })))
    }
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
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(context.workspace.root())
        .env_clear()
        .envs(environment)
        .kill_on_drop(true)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn()?;
    if let Some(stdin) = stdin
        && let Some(mut pipe) = child.stdin.take()
    {
        use tokio::io::AsyncWriteExt as _;
        pipe.write_all(&stdin).await?;
    }
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| ToolError::Io(std::io::Error::other("missing stdout")))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| ToolError::Io(std::io::Error::other("missing stderr")))?;
    let limit = context.max_output_bytes as u64 + 1;
    let stdout_task = tokio::spawn(async move {
        let mut output = Vec::new();
        stdout
            .take(limit)
            .read_to_end(&mut output)
            .await
            .map(|_| output)
    });
    let stderr_task = tokio::spawn(async move {
        let mut output = Vec::new();
        stderr
            .take(limit)
            .read_to_end(&mut output)
            .await
            .map(|_| output)
    });
    let status = if let Some(mut cancel) = context.cancel.clone() {
        tokio::select! {
            waited = timeout(context.timeout, child.wait()) => match waited {
                Ok(status) => status?,
                Err(_) => {
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    return Err(ToolError::Timeout);
                }
            },
            _ = cancel.wait() => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                return Err(ToolError::Cancelled);
            }
        }
    } else {
        match timeout(context.timeout, child.wait()).await {
            Ok(status) => status?,
            Err(_) => {
                let _ = child.kill().await;
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
    if stdout.len() > context.max_output_bytes || stderr.len() > context.max_output_bytes {
        return Err(ToolError::OutputTooLarge);
    }
    let mut stdout = String::from_utf8_lossy(&stdout).into_owned();
    let mut stderr = String::from_utf8_lossy(&stderr).into_owned();
    for secret in secret_values {
        stdout = stdout.replace(&secret, "[REDACTED]");
        stderr = stderr.replace(&secret, "[REDACTED]");
    }
    let stdout = mask_secrets(&stdout);
    let stderr = mask_secrets(&stderr);
    Ok(ToolResult {
        output: json!({"exitCode": status.code(), "success": status.success(), "stdout": stdout, "stderr": stderr}),
        is_error: !status.success(),
        error_code: (!status.success()).then(|| "process_failed".to_owned()),
    })
}
