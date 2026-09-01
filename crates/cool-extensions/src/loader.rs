use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use cool_security::Capability;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use url::Url;

use crate::capability;

pub const PLUGIN_SCHEMA: &str = "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json";
pub const MCP_SCHEMA: &str = "https://agent-plugins.org/schemas/1.0.0/mcp.schema.json";
pub const COOL_NAMESPACE: &str = "io.github.luckystrker.cool";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticLevel {
    Info,
    Warning,
    Error,
    Blocker,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Diagnostic {
    pub code: String,
    pub message: String,
    pub level: DiagnosticLevel,
    pub path: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginManifest {
    #[serde(rename = "$schema")]
    pub schema: String,
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub extensions: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
    pub allowed_tools: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "transport", rename_all = "kebab-case")]
pub enum McpServer {
    Stdio {
        name: String,
        command: PathBuf,
        args: Vec<String>,
        env: BTreeMap<String, String>,
        cwd: PathBuf,
    },
    StreamableHttp {
        name: String,
        url: String,
        headers: BTreeMap<String, String>,
    },
}

impl McpServer {
    pub fn name(&self) -> &str {
        match self {
            Self::Stdio { name, .. } | Self::StreamableHttp { name, .. } => name,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HookHandler {
    Command {
        command: PathBuf,
        args: Vec<String>,
        env: BTreeMap<String, String>,
        cwd: PathBuf,
    },
    Mcp {
        server: String,
        tool: String,
        arguments: BTreeMap<String, Value>,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HookDeclaration {
    pub id: String,
    pub event: String,
    pub handler: HookHandler,
    pub matcher: BTreeMap<String, Value>,
    pub order: i64,
    pub parallel: bool,
    pub capabilities: BTreeSet<CapabilityName>,
    pub trust_hash: String,
}

impl HookDeclaration {
    pub fn capability_set(&self) -> BTreeSet<Capability> {
        self.capabilities
            .iter()
            .filter_map(|item| capability(item.as_str()))
            .collect()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct CapabilityName(String);

impl CapabilityName {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompatibilityKind {
    Portable,
    Codex,
    Claude,
}

#[derive(Clone, Debug)]
pub struct PluginBundle {
    pub root: PathBuf,
    pub manifest: Option<PluginManifest>,
    pub compatibility: CompatibilityKind,
    pub content_hash: String,
    pub skills: Vec<Skill>,
    pub mcp_servers: Vec<McpServer>,
    pub hooks: Vec<HookDeclaration>,
    pub diagnostics: Vec<Diagnostic>,
}

impl PluginBundle {
    pub fn loadable(&self) -> bool {
        self.manifest.is_some()
            && !self
                .diagnostics
                .iter()
                .any(|item| item.level == DiagnosticLevel::Blocker)
    }

    pub fn conformant(&self) -> bool {
        self.compatibility == CompatibilityKind::Portable
            && self.loadable()
            && !self.diagnostics.iter().any(|item| {
                matches!(
                    item.level,
                    DiagnosticLevel::Error | DiagnosticLevel::Blocker
                ) && !item.path.starts_with(COOL_NAMESPACE)
            })
    }
}

#[derive(Debug)]
pub enum LoadError {
    Io(std::io::Error),
    Json(serde_json::Error),
    InvalidRoot,
}

impl fmt::Display for LoadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "plugin I/O error: {error}"),
            Self::Json(error) => write!(formatter, "plugin JSON error: {error}"),
            Self::InvalidRoot => formatter.write_str("plugin root is missing or link-like"),
        }
    }
}

impl std::error::Error for LoadError {}

impl From<std::io::Error> for LoadError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_json::Error> for LoadError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

#[derive(Clone, Default)]
pub struct PluginLoader;

impl PluginLoader {
    pub fn load(&self, root: &Path, data_root: &Path) -> Result<PluginBundle, LoadError> {
        if !root.is_dir() || is_link_like(&fs::symlink_metadata(root)?) {
            return Err(LoadError::InvalidRoot);
        }
        let root = root.canonicalize()?;
        let content_hash = hash_tree(&root)?;
        let (manifest_path, compatibility) = if root.join("plugin.json").is_file() {
            (root.join("plugin.json"), CompatibilityKind::Portable)
        } else if root.join(".codex-plugin/plugin.json").is_file() {
            (
                root.join(".codex-plugin/plugin.json"),
                CompatibilityKind::Codex,
            )
        } else if root.join(".claude-plugin/plugin.json").is_file() {
            (
                root.join(".claude-plugin/plugin.json"),
                CompatibilityKind::Claude,
            )
        } else if root.join("SKILL.md").is_file() {
            (root.join("SKILL.md"), CompatibilityKind::Claude)
        } else {
            return Ok(PluginBundle {
                root,
                manifest: None,
                compatibility: CompatibilityKind::Portable,
                content_hash,
                skills: Vec::new(),
                mcp_servers: Vec::new(),
                hooks: Vec::new(),
                diagnostics: vec![diagnostic(
                    "plugin.manifest_missing",
                    "plugin.json is missing",
                    DiagnosticLevel::Blocker,
                    "plugin.json",
                )],
            });
        };
        ensure_file_in(&root, &manifest_path)?;
        let raw: Value = if manifest_path.file_name().and_then(|value| value.to_str())
            == Some("SKILL.md")
        {
            let skill = parse_skill(&fs::read_to_string(&manifest_path)?, &manifest_path, false)
                .map_err(|_| LoadError::InvalidRoot)?;
            serde_json::json!({"name":skill.name,"description":skill.description})
        } else {
            serde_json::from_slice(&fs::read(&manifest_path)?)?
        };
        let mut diagnostics = Vec::new();
        let manifest = parse_manifest(&raw, compatibility, &mut diagnostics);
        if compatibility != CompatibilityKind::Portable {
            diagnostics.push(diagnostic(
                "compatibility.transformed",
                "vendor plugin layout is transformed into the portable internal model",
                DiagnosticLevel::Warning,
                manifest_path
                    .strip_prefix(&root)
                    .unwrap_or(&manifest_path)
                    .to_string_lossy(),
            ));
        }
        let mut bundle = PluginBundle {
            root: root.clone(),
            manifest,
            compatibility,
            content_hash,
            skills: Vec::new(),
            mcp_servers: Vec::new(),
            hooks: Vec::new(),
            diagnostics,
        };
        self.load_skills(&mut bundle)?;
        if compatibility == CompatibilityKind::Portable {
            self.load_mcp(&mut bundle, data_root)?;
            self.load_hooks(&mut bundle, data_root)?;
        } else {
            self.load_vendor_mcp(&mut bundle, data_root)?;
            self.inspect_vendor_features(&mut bundle)?;
        }
        Ok(bundle)
    }

    fn load_skills(&self, bundle: &mut PluginBundle) -> Result<(), LoadError> {
        let directory = bundle.root.join("skills");
        let mut paths = Vec::new();
        if directory.exists() {
            for entry in fs::read_dir(directory)? {
                paths.push(entry?.path().join("SKILL.md"));
            }
        }
        if bundle.compatibility == CompatibilityKind::Claude
            && bundle.root.join("SKILL.md").is_file()
        {
            paths.push(bundle.root.join("SKILL.md"));
        }
        for path in paths {
            if !path.is_file() || ensure_file_in(&bundle.root, &path).is_err() {
                continue;
            }
            let text = fs::read_to_string(&path)?;
            let enforce_directory_name = path != bundle.root.join("SKILL.md");
            match parse_skill(&text, &path, enforce_directory_name) {
                Ok(skill) => bundle.skills.push(skill),
                Err(message) => bundle.diagnostics.push(diagnostic(
                    "skill.invalid",
                    &message,
                    DiagnosticLevel::Error,
                    path.strip_prefix(&bundle.root)
                        .unwrap_or(&path)
                        .to_string_lossy(),
                )),
            }
        }
        bundle
            .skills
            .sort_by(|left, right| left.name.cmp(&right.name));
        Ok(())
    }

    fn load_vendor_mcp(
        &self,
        bundle: &mut PluginBundle,
        data_root: &Path,
    ) -> Result<(), LoadError> {
        let path = bundle.root.join(".mcp.json");
        if !path.exists() {
            return Ok(());
        }
        fs::create_dir_all(data_root)?;
        let data_root = data_root.canonicalize()?;
        ensure_file_in(&bundle.root, &path)?;
        let raw: Value = serde_json::from_slice(&fs::read(&path)?)?;
        let Some(servers) = raw
            .as_object()
            .filter(|object| object.keys().all(|key| key == "mcpServers"))
            .and_then(|object| object.get("mcpServers"))
            .and_then(Value::as_object)
        else {
            bundle.diagnostics.push(diagnostic(
                "compatibility.mcp_invalid",
                "vendor .mcp.json must contain only an mcpServers object",
                DiagnosticLevel::Error,
                ".mcp.json",
            ));
            return Ok(());
        };
        let mut transformed = 0_usize;
        for (name, value) in servers {
            let mut normalized = value.clone();
            if let Some(object) = normalized.as_object_mut()
                && !object.contains_key("type")
            {
                let transport = if object.contains_key("command") {
                    Some("stdio")
                } else if object.contains_key("url") {
                    Some("streamable-http")
                } else {
                    None
                };
                if let Some(transport) = transport {
                    object.insert("type".to_owned(), Value::String(transport.to_owned()));
                }
            }
            match parse_mcp(name, &normalized, &bundle.root, &data_root) {
                Ok(server) => {
                    transformed += 1;
                    bundle.mcp_servers.push(server);
                }
                Err(message) => bundle.diagnostics.push(diagnostic(
                    "compatibility.mcp_invalid",
                    &message,
                    DiagnosticLevel::Error,
                    format!(".mcp.json/mcpServers/{name}"),
                )),
            }
        }
        if transformed > 0 {
            bundle.diagnostics.push(diagnostic(
                "compatibility.mcp_transformed",
                "vendor .mcp.json was translated to the canonical MCP model",
                DiagnosticLevel::Warning,
                ".mcp.json",
            ));
        }
        Ok(())
    }

    fn inspect_vendor_features(&self, bundle: &mut PluginBundle) -> Result<(), LoadError> {
        let candidates: &[&str] = match bundle.compatibility {
            CompatibilityKind::Codex => &[".app.json", "hooks/hooks.json"],
            CompatibilityKind::Claude => &[
                "commands",
                "agents",
                "hooks/hooks.json",
                "settings.json",
                "monitors",
                "themes",
                "bin",
            ],
            CompatibilityKind::Portable => &[],
        };
        for relative in candidates {
            let path = bundle.root.join(relative);
            if path.exists() {
                bundle.diagnostics.push(diagnostic(
                    "compatibility.feature_unsupported",
                    "vendor feature is detected but remains inactive until a reviewed semantic mapping exists",
                    DiagnosticLevel::Warning,
                    *relative,
                ));
            }
        }
        Ok(())
    }

    fn load_mcp(&self, bundle: &mut PluginBundle, data_root: &Path) -> Result<(), LoadError> {
        let path = bundle.root.join("mcp.json");
        if !path.exists() {
            return Ok(());
        }
        fs::create_dir_all(data_root)?;
        let data_root = data_root.canonicalize()?;
        ensure_file_in(&bundle.root, &path)?;
        let raw: Value = serde_json::from_slice(&fs::read(&path)?)?;
        let Some(document) = raw.as_object() else {
            bundle.diagnostics.push(diagnostic(
                "mcp.document_invalid",
                "mcp.json must be an object",
                DiagnosticLevel::Error,
                "mcp.json",
            ));
            return Ok(());
        };
        if document.get("$schema").and_then(Value::as_str) != Some(MCP_SCHEMA)
            || document
                .keys()
                .any(|key| key != "$schema" && key != "mcpServers")
        {
            bundle.diagnostics.push(diagnostic(
                "mcp.document_invalid",
                "mcp.json must use the pinned Agent Plugins 1.0 schema",
                DiagnosticLevel::Error,
                "mcp.json",
            ));
            return Ok(());
        }
        let Some(servers) = document.get("mcpServers").and_then(Value::as_object) else {
            bundle.diagnostics.push(diagnostic(
                "mcp.document_invalid",
                "mcpServers must be an object",
                DiagnosticLevel::Error,
                "mcp.json/mcpServers",
            ));
            return Ok(());
        };
        for (name, value) in servers {
            match parse_mcp(name, value, &bundle.root, &data_root) {
                Ok(server) => bundle.mcp_servers.push(server),
                Err(message) => bundle.diagnostics.push(diagnostic(
                    "mcp.server_invalid",
                    &message,
                    DiagnosticLevel::Error,
                    format!("mcp.json/mcpServers/{name}"),
                )),
            }
        }
        bundle
            .mcp_servers
            .sort_by(|left, right| left.name().cmp(right.name()));
        Ok(())
    }

    fn load_hooks(&self, bundle: &mut PluginBundle, data_root: &Path) -> Result<(), LoadError> {
        let path = bundle.root.join(COOL_NAMESPACE).join("hooks/hooks.json");
        if !path.exists() {
            return Ok(());
        }
        fs::create_dir_all(data_root)?;
        let data_root = data_root.canonicalize()?;
        ensure_file_in(&bundle.root, &path)?;
        let raw: Value = serde_json::from_slice(&fs::read(&path)?)?;
        let Some(items) = raw
            .get("hooks")
            .and_then(Value::as_array)
            .filter(|_| raw.get("version").and_then(Value::as_u64) == Some(1))
        else {
            bundle.diagnostics.push(diagnostic(
                "hooks.document_invalid",
                "hooks.json must contain version 1 and hooks",
                DiagnosticLevel::Error,
                format!("{COOL_NAMESPACE}/hooks/hooks.json"),
            ));
            return Ok(());
        };
        let mut ids = BTreeSet::new();
        for (index, item) in items.iter().enumerate() {
            match parse_hook(item, &bundle.root, &data_root, &bundle.content_hash) {
                Ok(hook) if ids.insert(hook.id.clone()) => bundle.hooks.push(hook),
                Ok(_) => bundle.diagnostics.push(diagnostic(
                    "hook.duplicate_id",
                    "hook id must be unique",
                    DiagnosticLevel::Error,
                    format!("{COOL_NAMESPACE}/hooks/hooks.json/hooks/{index}"),
                )),
                Err(message) => bundle.diagnostics.push(diagnostic(
                    "hook.invalid",
                    &message,
                    DiagnosticLevel::Error,
                    format!("{COOL_NAMESPACE}/hooks/hooks.json/hooks/{index}"),
                )),
            }
        }
        bundle.hooks.sort_by(|left, right| {
            left.order
                .cmp(&right.order)
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok(())
    }
}

fn parse_manifest(
    raw: &Value,
    compatibility: CompatibilityKind,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<PluginManifest> {
    let object = raw.as_object()?;
    let allowed = [
        "$schema",
        "name",
        "version",
        "description",
        "author",
        "homepage",
        "repository",
        "license",
        "keywords",
        "extensions",
    ];
    if compatibility == CompatibilityKind::Portable
        && object.keys().any(|key| !allowed.contains(&key.as_str()))
    {
        diagnostics.push(diagnostic(
            "manifest.unknown_field",
            "portable manifest contains an unknown field",
            DiagnosticLevel::Blocker,
            "plugin.json",
        ));
        return None;
    }
    if compatibility == CompatibilityKind::Portable && !portable_manifest_types_valid(object) {
        diagnostics.push(diagnostic(
            "manifest.type_invalid",
            "portable manifest fields do not match the pinned schema",
            DiagnosticLevel::Blocker,
            "plugin.json",
        ));
        return None;
    }
    let name = object.get("name")?.as_str()?.trim();
    if !valid_name(name) {
        diagnostics.push(diagnostic(
            "manifest.name_invalid",
            "plugin name is invalid",
            DiagnosticLevel::Blocker,
            "plugin.json/name",
        ));
        return None;
    }
    let schema = object.get("$schema").and_then(Value::as_str);
    if compatibility == CompatibilityKind::Portable && schema != Some(PLUGIN_SCHEMA) {
        diagnostics.push(diagnostic(
            "manifest.schema_invalid",
            "plugin schema is not the pinned Agent Plugins 1.0 schema",
            DiagnosticLevel::Blocker,
            "plugin.json/$schema",
        ));
        return None;
    }
    Some(PluginManifest {
        schema: schema.unwrap_or(PLUGIN_SCHEMA).to_owned(),
        name: name.to_owned(),
        version: string_field(object, "version"),
        description: string_field(object, "description"),
        extensions: object
            .get("extensions")
            .and_then(Value::as_object)
            .map(|map| {
                map.iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect()
            })
            .unwrap_or_default(),
    })
}

fn parse_skill(text: &str, path: &Path, enforce_directory_name: bool) -> Result<Skill, String> {
    let normalized = text.replace("\r\n", "\n");
    let rest = normalized
        .strip_prefix("---\n")
        .ok_or("SKILL.md must begin with YAML frontmatter")?;
    let end = rest
        .find("\n---\n")
        .ok_or("SKILL.md frontmatter is not closed")?;
    let mut fields = BTreeMap::new();
    let mut block_field: Option<&str> = None;
    for line in rest[..end].lines() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        if line.starts_with(' ') || line.starts_with('\t') {
            if block_field == Some("metadata") {
                let (key, value) = line
                    .trim()
                    .split_once(':')
                    .ok_or("metadata entries must be string key/value pairs")?;
                if key.trim().is_empty() || yaml_scalar(value).is_empty() {
                    return Err("metadata entries must be string key/value pairs".to_owned());
                }
                continue;
            }
            return Err("frontmatter contains an unexpected indented value".to_owned());
        }
        let (key, value) = line
            .split_once(':')
            .ok_or("frontmatter entries must be key/value pairs")?;
        let key = key.trim();
        if !matches!(
            key,
            "name" | "description" | "license" | "compatibility" | "metadata" | "allowed-tools"
        ) || fields.insert(key, value.trim()).is_some()
        {
            return Err("frontmatter contains an unknown or duplicate field".to_owned());
        }
        block_field = value.trim().is_empty().then_some(key);
    }
    let name = fields
        .get("name")
        .map(|value| yaml_scalar(value))
        .unwrap_or_default();
    let description = fields
        .get("description")
        .map(|value| yaml_scalar(value))
        .unwrap_or_default();
    if !valid_name(name) || description.is_empty() {
        return Err("skill requires a valid name and non-empty description".to_owned());
    }
    if enforce_directory_name
        && path
            .parent()
            .and_then(Path::file_name)
            .and_then(|value| value.to_str())
            != Some(name)
    {
        return Err("skill name must match its parent directory".to_owned());
    }
    if description.len() > 1024 {
        return Err("skill description exceeds 1024 characters".to_owned());
    }
    if fields
        .get("compatibility")
        .is_some_and(|value| yaml_scalar(value).is_empty() || yaml_scalar(value).len() > 500)
        || fields
            .get("license")
            .is_some_and(|value| yaml_scalar(value).is_empty())
    {
        return Err("skill license or compatibility is invalid".to_owned());
    }
    let allowed_tools = fields
        .get("allowed-tools")
        .map(|value| {
            let value = yaml_scalar(value);
            if value.is_empty() || value.starts_with('[') {
                return Err("allowed-tools must be a non-empty space-separated string".to_owned());
            }
            Ok(value.split_whitespace().map(str::to_owned).collect())
        })
        .transpose()?
        .unwrap_or_default();
    Ok(Skill {
        name: name.to_owned(),
        description: description.to_owned(),
        path: path.to_path_buf(),
        allowed_tools,
    })
}

fn yaml_scalar(value: &str) -> &str {
    value
        .trim()
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .or_else(|| {
            value
                .trim()
                .strip_prefix('\'')
                .and_then(|value| value.strip_suffix('\''))
        })
        .unwrap_or_else(|| value.trim())
}

fn parse_mcp(name: &str, raw: &Value, root: &Path, data_root: &Path) -> Result<McpServer, String> {
    if !valid_name(name) {
        return Err("server name is invalid".to_owned());
    }
    let object = raw.as_object().ok_or("server must be an object")?;
    match object.get("type").and_then(Value::as_str) {
        Some("stdio") => {
            reject_fields(object, &["type", "command", "args", "env", "cwd"])?;
            let command = object
                .get("command")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or("stdio command is required")?;
            let command = if command.starts_with("./") {
                expand_path(command, root, data_root)?
            } else if command.contains('/') || command.contains('\\') {
                return Err(
                    "stdio command must be a bare executable name or start with ./".to_owned(),
                );
            } else {
                PathBuf::from(command)
            };
            if command.is_absolute() && command.starts_with(root) {
                ensure_file_in(root, &command).map_err(|error| error.to_string())?;
            }
            let args = string_array(object.get("args"))?
                .into_iter()
                .map(|value| expand_value(&value, root, data_root))
                .collect();
            let mut env = string_map(object.get("env"))?
                .into_iter()
                .map(|(key, value)| (key, expand_value(&value, root, data_root)))
                .collect::<BTreeMap<_, _>>();
            if env.keys().any(|key| {
                key.eq_ignore_ascii_case("PLUGIN_ROOT") || key.eq_ignore_ascii_case("PLUGIN_DATA")
            }) {
                return Err("reserved plugin environment name".to_owned());
            }
            env.insert(
                "PLUGIN_ROOT".to_owned(),
                root.to_string_lossy().into_owned(),
            );
            env.insert(
                "PLUGIN_DATA".to_owned(),
                data_root.to_string_lossy().into_owned(),
            );
            let cwd = object
                .get("cwd")
                .and_then(Value::as_str)
                .map(|value| expand_path(value, root, data_root))
                .transpose()?
                .unwrap_or_else(|| root.to_path_buf());
            if !cwd.starts_with(root) && !cwd.starts_with(data_root) {
                return Err("stdio cwd escapes plugin roots".to_owned());
            }
            fs::create_dir_all(&cwd).map_err(|error| error.to_string())?;
            let cwd = cwd.canonicalize().map_err(|error| error.to_string())?;
            if !cwd.starts_with(root) && !cwd.starts_with(data_root) {
                return Err("stdio cwd resolves through a link outside plugin roots".to_owned());
            }
            Ok(McpServer::Stdio {
                name: name.to_owned(),
                command,
                args,
                env,
                cwd,
            })
        }
        Some("streamable-http") => {
            reject_fields(object, &["type", "url", "headers"])?;
            let url = object
                .get("url")
                .and_then(Value::as_str)
                .ok_or("HTTP URL is required")?
                .parse::<Url>()
                .map_err(|error| error.to_string())?;
            let host = url.host_str().ok_or("HTTP URL needs a host")?;
            let loopback = host.eq_ignore_ascii_case("localhost")
                || host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|address| address.is_loopback());
            if !matches!(url.scheme(), "http" | "https")
                || !url.username().is_empty()
                || url.password().is_some()
                || url.fragment().is_some()
                || (url.scheme() != "https" && !loopback)
            {
                return Err("HTTP URL must be credential-free HTTPS (or exact loopback HTTP) without a fragment".to_owned());
            }
            let headers = string_map(object.get("headers"))?;
            validate_headers(&headers)?;
            Ok(McpServer::StreamableHttp {
                name: name.to_owned(),
                url: url.to_string(),
                headers,
            })
        }
        Some("sse") => Err("legacy SSE transport is declared but unsupported by M8".to_owned()),
        _ => Err("server type must be stdio or streamable-http".to_owned()),
    }
}

fn parse_hook(
    raw: &Value,
    root: &Path,
    data_root: &Path,
    content_hash: &str,
) -> Result<HookDeclaration, String> {
    let object = raw.as_object().ok_or("hook must be an object")?;
    reject_fields(
        object,
        &[
            "id",
            "event",
            "handler",
            "matcher",
            "order",
            "concurrency",
            "capabilities",
        ],
    )?;
    let id = object
        .get("id")
        .and_then(Value::as_str)
        .filter(|value| valid_name(value))
        .ok_or("hook id is invalid")?
        .to_owned();
    let event = object
        .get("event")
        .and_then(Value::as_str)
        .filter(|value| {
            matches!(
                *value,
                "SessionStart"
                    | "SessionEnd"
                    | "UserPromptSubmit"
                    | "PreToolUse"
                    | "PermissionRequest"
                    | "PostToolUse"
                    | "PreCompact"
                    | "PostCompact"
                    | "SubagentStart"
                    | "SubagentStop"
                    | "Stop"
                    | "Interrupt"
            )
        })
        .ok_or("hook event is unsupported")?
        .to_owned();
    let handler_object = object
        .get("handler")
        .and_then(Value::as_object)
        .ok_or("hook handler is required")?;
    let handler = match handler_object.get("type").and_then(Value::as_str) {
        Some("command") => {
            reject_fields(handler_object, &["type", "command", "args", "env"])?;
            let raw_command = handler_object
                .get("command")
                .and_then(Value::as_str)
                .ok_or("hook command is required")?;
            let command = if raw_command.starts_with("./") {
                expand_path(raw_command, root, data_root)?
            } else if raw_command.contains('/') || raw_command.contains('\\') {
                return Err(
                    "hook command must be a bare executable name or start with ./".to_owned(),
                );
            } else {
                PathBuf::from(raw_command)
            };
            if command.is_absolute() && command.starts_with(root) {
                ensure_file_in(root, &command).map_err(|error| error.to_string())?;
            }
            let args = string_array(handler_object.get("args"))?
                .into_iter()
                .map(|value| expand_value(&value, root, data_root))
                .collect();
            let mut env = string_map(handler_object.get("env"))?
                .into_iter()
                .map(|(key, value)| (key, expand_value(&value, root, data_root)))
                .collect::<BTreeMap<_, _>>();
            if env.keys().any(|key| {
                key.eq_ignore_ascii_case("PLUGIN_ROOT") || key.eq_ignore_ascii_case("PLUGIN_DATA")
            }) {
                return Err("reserved plugin environment name".to_owned());
            }
            env.insert(
                "PLUGIN_ROOT".to_owned(),
                root.to_string_lossy().into_owned(),
            );
            env.insert(
                "PLUGIN_DATA".to_owned(),
                data_root.to_string_lossy().into_owned(),
            );
            HookHandler::Command {
                command,
                args,
                env,
                cwd: root.to_path_buf(),
            }
        }
        Some("mcp") => {
            reject_fields(handler_object, &["type", "server", "tool", "arguments"])?;
            HookHandler::Mcp {
                server: handler_object
                    .get("server")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .ok_or("MCP hook server is required")?
                    .to_owned(),
                tool: handler_object
                    .get("tool")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .ok_or("MCP hook tool is required")?
                    .to_owned(),
                arguments: handler_object
                    .get("arguments")
                    .map(|value| {
                        value
                            .as_object()
                            .ok_or("MCP hook arguments must be an object")
                            .map(|items| {
                                items
                                    .iter()
                                    .map(|(key, value)| (key.clone(), value.clone()))
                                    .collect()
                            })
                    })
                    .transpose()?
                    .unwrap_or_default(),
            }
        }
        _ => return Err("hook handler type must be command or mcp".to_owned()),
    };
    let matcher = object
        .get("matcher")
        .and_then(Value::as_object)
        .map(|value| {
            value
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect()
        })
        .unwrap_or_default();
    let order = object.get("order").and_then(Value::as_i64).unwrap_or(0);
    let parallel = match object
        .get("concurrency")
        .and_then(Value::as_str)
        .unwrap_or("serial")
    {
        "serial" => false,
        "parallel" => true,
        _ => return Err("hook concurrency is invalid".to_owned()),
    };
    let mut capabilities = BTreeSet::new();
    if let Some(items) = object.get("capabilities") {
        for value in items
            .as_array()
            .ok_or("hook capabilities must be an array")?
        {
            let name = value.as_str().ok_or("hook capability must be a string")?;
            if capability(name).is_none() || !capabilities.insert(CapabilityName(name.to_owned())) {
                return Err("hook capability is unknown or duplicate".to_owned());
            }
        }
    }
    match &handler {
        HookHandler::Command { .. } => {
            capabilities.insert(CapabilityName("execute".to_owned()));
        }
        HookHandler::Mcp { .. } => {
            // The declaration does not bind a transport here. Require both conservative
            // out-of-process capabilities; a later core-owned policy may narrow further.
            capabilities.insert(CapabilityName("execute".to_owned()));
            capabilities.insert(CapabilityName("network".to_owned()));
        }
    }
    let normalized = serde_json::json!({"contentHash": content_hash, "id": id, "event": event, "handler": handler, "matcher": matcher, "order": order, "parallel": parallel, "capabilities": capabilities});
    let trust_hash = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&normalized).map_err(|error| error.to_string())?)
    );
    Ok(HookDeclaration {
        id,
        event,
        handler,
        matcher,
        order,
        parallel,
        capabilities,
        trust_hash,
    })
}

fn hash_tree(root: &Path) -> Result<String, LoadError> {
    let mut paths = Vec::new();
    collect_files(root, root, &mut paths)?;
    let mut files = paths
        .into_iter()
        .map(|path| {
            let relative = path
                .strip_prefix(root)
                .map_err(|_| LoadError::InvalidRoot)?
                .to_string_lossy()
                .replace('\\', "/");
            Ok((relative, path))
        })
        .collect::<Result<Vec<_>, LoadError>>()?;
    files.sort_by(|left, right| left.0.cmp(&right.0));
    let mut hash = Sha256::new();
    for (relative, path) in files {
        let bytes = fs::read(path)?;
        hash.update(format!("F\0{relative}\0{}\0", bytes.len()).as_bytes());
        hash.update(bytes);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn collect_files(
    root: &Path,
    directory: &Path,
    output: &mut Vec<PathBuf>,
) -> Result<(), LoadError> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        if is_link_like(&metadata) {
            return Err(LoadError::InvalidRoot);
        }
        if metadata.is_dir() {
            collect_files(root, &entry.path(), output)?;
        } else if metadata.is_file() {
            ensure_file_in(root, &entry.path())?;
            output.push(entry.path());
        }
    }
    Ok(())
}

fn ensure_file_in(root: &Path, path: &Path) -> Result<(), LoadError> {
    let canonical = path.canonicalize()?;
    if !canonical.starts_with(root) || is_link_like(&fs::symlink_metadata(path)?) {
        return Err(LoadError::InvalidRoot);
    }
    Ok(())
}

fn is_link_like(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn expand_path(value: &str, root: &Path, data: &Path) -> Result<PathBuf, String> {
    let path = if let Some(rest) = value.strip_prefix("${PLUGIN_ROOT}") {
        root.join(rest.trim_start_matches(['/', '\\']))
    } else if let Some(rest) = value.strip_prefix("${PLUGIN_DATA}") {
        data.join(rest.trim_start_matches(['/', '\\']))
    } else if let Some(rest) = value.strip_prefix("./") {
        root.join(rest)
    } else {
        PathBuf::from(value)
    };
    let normalized = normalize_without_links(&path)?;
    if (value.starts_with("./") || value.starts_with("${PLUGIN_ROOT}"))
        && !normalized.starts_with(root)
    {
        return Err("path escapes plugin root".to_owned());
    }
    if value.starts_with("${PLUGIN_DATA}") && !normalized.starts_with(data) {
        return Err("path escapes plugin data root".to_owned());
    }
    Ok(normalized)
}

fn expand_value(value: &str, root: &Path, data: &Path) -> String {
    value
        .replace("${PLUGIN_ROOT}", &root.to_string_lossy())
        .replace("${PLUGIN_DATA}", &data.to_string_lossy())
}

fn normalize_without_links(path: &Path) -> Result<PathBuf, String> {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                if !result.pop() {
                    return Err("path escapes root".to_owned());
                }
            }
            std::path::Component::CurDir => {}
            other => result.push(other.as_os_str()),
        }
    }
    Ok(result)
}

fn reject_fields(object: &serde_json::Map<String, Value>, allowed: &[&str]) -> Result<(), String> {
    if let Some(key) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        Err(format!("unknown field {key}"))
    } else {
        Ok(())
    }
}

fn string_array(value: Option<&Value>) -> Result<Vec<String>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    value
        .as_array()
        .ok_or_else(|| "value must be an array".to_owned())?
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_owned)
                .ok_or_else(|| "array values must be strings".to_owned())
        })
        .collect()
}

fn string_map(value: Option<&Value>) -> Result<BTreeMap<String, String>, String> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    value
        .as_object()
        .ok_or_else(|| "value must be an object".to_owned())?
        .iter()
        .map(|(key, value)| {
            value
                .as_str()
                .map(|value| (key.clone(), value.to_owned()))
                .ok_or_else(|| "object values must be strings".to_owned())
        })
        .collect()
}

fn validate_headers(headers: &BTreeMap<String, String>) -> Result<(), String> {
    const RESERVED: &[&str] = &[
        "host",
        "content-length",
        "transfer-encoding",
        "connection",
        "proxy-connection",
        "keep-alive",
        "upgrade",
        "te",
        "trailer",
        "mcp-session-id",
        "mcp-protocol-version",
        "accept",
        "content-type",
    ];
    let mut names = BTreeSet::new();
    for (name, value) in headers {
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
            || value.chars().any(char::is_control)
            || !names.insert(name.to_ascii_lowercase())
            || RESERVED.contains(&name.to_ascii_lowercase().as_str())
        {
            return Err("HTTP headers are invalid or duplicate case-insensitively".to_owned());
        }
    }
    Ok(())
}

fn valid_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && !value.contains("..")
        && !value.contains("--")
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-')
        })
        && value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && value
            .as_bytes()
            .last()
            .is_some_and(u8::is_ascii_alphanumeric)
}

fn portable_manifest_types_valid(object: &serde_json::Map<String, Value>) -> bool {
    for key in [
        "version",
        "description",
        "homepage",
        "repository",
        "license",
    ] {
        if object.get(key).is_some_and(|value| !value.is_string()) {
            return false;
        }
    }
    if object.get("keywords").is_some_and(|value| {
        value
            .as_array()
            .is_none_or(|items| items.iter().any(|item| !item.is_string()))
    }) {
        return false;
    }
    if object.get("extensions").is_some_and(|value| {
        value
            .as_object()
            .is_none_or(|items| items.values().any(|item| !item.is_object()))
    }) {
        return false;
    }
    if let Some(author) = object.get("author") {
        let Some(author) = author.as_object() else {
            return false;
        };
        if author
            .keys()
            .any(|key| !matches!(key.as_str(), "name" | "email" | "url"))
            || author.values().any(|value| !value.is_string())
        {
            return false;
        }
    }
    true
}

fn string_field(object: &serde_json::Map<String, Value>, key: &str) -> String {
    object
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn diagnostic(
    code: &str,
    message: &str,
    level: DiagnosticLevel,
    path: impl fmt::Display,
) -> Diagnostic {
    Diagnostic {
        code: code.to_owned(),
        message: message.to_owned(),
        level,
        path: path.to_string(),
    }
}
