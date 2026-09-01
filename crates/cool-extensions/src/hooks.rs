use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cool_security::{CapabilityPolicy, Decision, mask_json, sanitize_environment};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::process::Command;
use tokio::time::timeout;

use crate::{HookDeclaration, HookHandler, McpClient, narrowed_plugin_policy};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HookInvocation {
    pub event: String,
    pub fields: BTreeMap<String, Value>,
    pub payload: Value,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HookAudit {
    pub plugin: String,
    pub hook_id: String,
    pub trust_hash: String,
    pub outcome: String,
    pub detail: Option<String>,
}

#[derive(Clone, Default)]
pub struct HookReviewStore {
    values: Arc<Mutex<BTreeMap<(String, String), String>>>,
}

impl HookReviewStore {
    pub fn approve(&self, plugin: &str, hook: &str, trust_hash: &str) -> Result<(), HookError> {
        self.values
            .lock()
            .map_err(|_| HookError::Poisoned)?
            .insert((plugin.to_owned(), hook.to_owned()), trust_hash.to_owned());
        Ok(())
    }

    pub fn is_approved(&self, plugin: &str, hook: &HookDeclaration) -> Result<bool, HookError> {
        Ok(self
            .values
            .lock()
            .map_err(|_| HookError::Poisoned)?
            .get(&(plugin.to_owned(), hook.id.clone()))
            .is_some_and(|value| value == &hook.trust_hash))
    }
}

#[derive(Debug)]
pub enum HookError {
    Unreviewed,
    ApprovalRequired,
    Denied,
    Io(std::io::Error),
    Mcp(crate::McpError),
    Json(serde_json::Error),
    Timeout,
    Failed(String),
    Poisoned,
}

impl fmt::Display for HookError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreviewed => formatter.write_str("hook is untrusted or changed since review"),
            Self::ApprovalRequired => formatter.write_str("hook requires approval"),
            Self::Denied => formatter.write_str("hook is denied by policy"),
            Self::Io(error) => write!(formatter, "hook I/O error: {error}"),
            Self::Mcp(error) => write!(formatter, "hook MCP error: {error}"),
            Self::Json(error) => write!(formatter, "hook JSON error: {error}"),
            Self::Timeout => formatter.write_str("hook timed out"),
            Self::Failed(message) => write!(formatter, "hook failed: {message}"),
            Self::Poisoned => formatter.write_str("hook state lock is poisoned"),
        }
    }
}

impl std::error::Error for HookError {}
impl From<std::io::Error> for HookError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}
impl From<serde_json::Error> for HookError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}
impl From<crate::McpError> for HookError {
    fn from(value: crate::McpError) -> Self {
        Self::Mcp(value)
    }
}

#[derive(Clone)]
pub struct HookEngine {
    reviews: HookReviewStore,
    mcp: Arc<HashMap<String, McpClient>>,
    audit_path: PathBuf,
    audit_lock: Arc<Mutex<()>>,
    timeout: Duration,
    max_output_bytes: usize,
    allow_trusted_host_processes: bool,
}

impl HookEngine {
    pub fn new(
        reviews: HookReviewStore,
        mcp: HashMap<String, McpClient>,
        audit_path: PathBuf,
    ) -> Self {
        Self {
            reviews,
            mcp: Arc::new(mcp),
            audit_path,
            audit_lock: Arc::new(Mutex::new(())),
            timeout: Duration::from_secs(30),
            max_output_bytes: 1_048_576,
            allow_trusted_host_processes: false,
        }
    }

    /// Explicit single-user trusted-host opt-in. Production embeddings leave this disabled until
    /// they provide an OS-isolated launcher.
    pub fn with_trusted_host_processes(mut self, allowed: bool) -> Self {
        self.allow_trusted_host_processes = allowed;
        self
    }

    pub async fn invoke(
        &self,
        plugin: &str,
        hook: &HookDeclaration,
        invocation: &HookInvocation,
        core_policy: &CapabilityPolicy,
        plugin_policy: &CapabilityPolicy,
    ) -> Result<Value, HookError> {
        if hook.event != invocation.event || !matches_fields(&hook.matcher, &invocation.fields) {
            return Ok(Value::Null);
        }
        if !self.reviews.is_approved(plugin, hook)? {
            self.audit(HookAudit {
                plugin: plugin.to_owned(),
                hook_id: hook.id.clone(),
                trust_hash: hook.trust_hash.clone(),
                outcome: "blocked_unreviewed".to_owned(),
                detail: None,
            })?;
            return Err(HookError::Unreviewed);
        }
        let policy = narrowed_plugin_policy(core_policy, &hook.capability_set(), plugin_policy);
        let decision = policy
            .evaluate(hook.capability_set(), Decision::Allow)
            .effective;
        match decision {
            Decision::Deny => {
                self.audit_block(plugin, hook, "blocked_policy", None)?;
                return Err(HookError::Denied);
            }
            Decision::Ask => {
                self.audit_block(plugin, hook, "awaiting_approval", None)?;
                return Err(HookError::ApprovalRequired);
            }
            Decision::Allow => {}
        }
        let result = match &hook.handler {
            HookHandler::Command {
                command,
                args,
                env,
                cwd,
            } => {
                if !self.allow_trusted_host_processes {
                    self.audit_block(
                        plugin,
                        hook,
                        "blocked_launcher",
                        Some("OS-isolated command launcher is not configured".to_owned()),
                    )?;
                    return Err(HookError::Denied);
                }
                self.command(command, args, env, cwd, &invocation.payload)
                    .await
            }
            HookHandler::Mcp {
                server,
                tool,
                arguments,
            } => {
                let scoped = format!("{plugin}/{server}");
                let client = self
                    .mcp
                    .get(&scoped)
                    .or_else(|| self.mcp.get(server))
                    .ok_or_else(|| HookError::Failed(format!("unknown MCP server {server}")))?;
                let mut payload = invocation.payload.as_object().cloned().unwrap_or_default();
                payload.extend(
                    arguments
                        .iter()
                        .map(|(key, value)| (key.clone(), value.clone())),
                );
                client
                    .call_tool(tool, Value::Object(payload))
                    .await
                    .map_err(HookError::from)
            }
        };
        let (outcome, detail) = match &result {
            Ok(_) => ("completed".to_owned(), None),
            Err(error) => ("failed".to_owned(), Some(error.to_string())),
        };
        self.audit(HookAudit {
            plugin: plugin.to_owned(),
            hook_id: hook.id.clone(),
            trust_hash: hook.trust_hash.clone(),
            outcome,
            detail,
        })?;
        result
    }

    fn audit(&self, mut entry: HookAudit) -> Result<(), HookError> {
        if let Some(detail) = entry.detail.as_mut() {
            *detail = cool_security::mask_secrets(detail);
        }
        let _guard = self.audit_lock.lock().map_err(|_| HookError::Poisoned)?;
        if let Some(parent) = self.audit_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut encoded = serde_json::to_vec(&entry)?;
        encoded.push(b'\n');
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.audit_path)?;
        file.write_all(&encoded)?;
        file.flush()?;
        Ok(())
    }

    fn audit_block(
        &self,
        plugin: &str,
        hook: &HookDeclaration,
        outcome: &str,
        detail: Option<String>,
    ) -> Result<(), HookError> {
        self.audit(HookAudit {
            plugin: plugin.to_owned(),
            hook_id: hook.id.clone(),
            trust_hash: hook.trust_hash.clone(),
            outcome: outcome.to_owned(),
            detail,
        })
    }

    async fn command(
        &self,
        command: &Path,
        args: &[String],
        environment: &BTreeMap<String, String>,
        cwd: &Path,
        payload: &Value,
    ) -> Result<Value, HookError> {
        let mut safe_environment = crate::execution_environment_baseline();
        safe_environment.extend(sanitize_environment(
            environment
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str())),
            &Default::default(),
        ));
        let mut child = Command::new(command)
            .args(args)
            .current_dir(cwd)
            .env_clear()
            .envs(safe_environment)
            .kill_on_drop(true)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(&serde_json::to_vec(payload)?).await?;
        }
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| HookError::Failed("missing stdout".to_owned()))?
            .take(self.max_output_bytes as u64 + 1);
        let mut stderr = child
            .stderr
            .take()
            .ok_or_else(|| HookError::Failed("missing stderr".to_owned()))?
            .take(self.max_output_bytes as u64 + 1);
        let stdout_task = tokio::spawn(async move {
            let mut value = Vec::new();
            stdout.read_to_end(&mut value).await.map(|_| value)
        });
        let stderr_task = tokio::spawn(async move {
            let mut value = Vec::new();
            stderr.read_to_end(&mut value).await.map(|_| value)
        });
        let status = match timeout(self.timeout, child.wait()).await {
            Ok(status) => status?,
            Err(_) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                return Err(HookError::Timeout);
            }
        };
        let stdout = stdout_task
            .await
            .map_err(|error| HookError::Failed(error.to_string()))??;
        let stderr = stderr_task
            .await
            .map_err(|error| HookError::Failed(error.to_string()))??;
        if stdout.len() > self.max_output_bytes || stderr.len() > self.max_output_bytes {
            return Err(HookError::Failed("hook output is too large".to_owned()));
        }
        if !status.success() {
            return Err(HookError::Failed(cool_security::mask_secrets(
                &String::from_utf8_lossy(&stderr),
            )));
        }
        let mut output = if stdout.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&stdout)?
        };
        mask_json(&mut output);
        Ok(output)
    }
}

fn matches_fields(matcher: &BTreeMap<String, Value>, fields: &BTreeMap<String, Value>) -> bool {
    matcher
        .iter()
        .all(|(key, value)| fields.get(key) == Some(value))
}
