//! Operator-owned global skills store (M11 WS1d / Workstream B2).
//!
//! Python discovered skills from several filesystem roots (builtin/user/plugin).
//! The Rust core already projects plugin-bundled skills through
//! `extensions.status`; this module adds the operator-managed global store the
//! web admin writes: a `<data-dir>/skills/<name>/SKILL.md` tree parsed with the
//! same permissive YAML-subset front matter as Python's `skills/models.py`.
//! A skill body is instructions, never executed here.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use async_trait::async_trait;
use cool_app_server::SkillAdmin;
use cool_protocol::{SkillAdminRecord, SkillCreateParams, SkillCreateResult, SkillListResult};
use serde_json::Value;

const AUDIT_FILE: &str = "skills-admin-audit.jsonl";

/// A parsed `SKILL.md`.
#[derive(Clone, Debug, PartialEq)]
struct SkillFile {
    name: String,
    description: String,
    tags: Vec<String>,
    tools: Vec<String>,
    version: String,
    body: String,
}

/// The operator skills store rooted at `<data-dir>/skills`.
pub struct CliSkillAdmin {
    root: PathBuf,
    audit_path: PathBuf,
    /// Serializes create/delete so two concurrent writes cannot race a name.
    lock: Mutex<()>,
}

impl CliSkillAdmin {
    pub fn new(data_dir: impl AsRef<Path>) -> Self {
        let data_dir = data_dir.as_ref();
        Self {
            root: data_dir.join("skills"),
            audit_path: data_dir.join(AUDIT_FILE),
            lock: Mutex::new(()),
        }
    }

    fn audit(&self, actor: &str, action: &str, target: &str, outcome: &str) {
        let record = serde_json::json!({
            "at": now_string(),
            "actor": actor,
            "action": action,
            "target": target,
            "outcome": outcome,
        });
        if let Ok(line) = serde_json::to_string(&record) {
            use std::io::Write as _;
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.audit_path)
            {
                let _ = writeln!(file, "{line}");
            }
        }
    }

    fn list_records(&self, source: Option<&str>) -> Result<Vec<SkillAdminRecord>, String> {
        // The Rust store is a single directory; both Python scopes resolve to
        // it, so `user`/`global` are accepted as source aliases and any other
        // source (plugin/builtin) is empty.
        if let Some(source) = source
            && !matches!(source, "user" | "global")
        {
            return Ok(Vec::new());
        }
        let mut skills = Vec::new();
        if self.root.is_dir() {
            let entries = std::fs::read_dir(&self.root)
                .map_err(|error| format!("skills directory unreadable: {error}"))?;
            for entry in entries {
                let entry = entry.map_err(|error| format!("skills entry unreadable: {error}"))?;
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                let directory = entry.file_name().to_string_lossy().into_owned();
                if directory.starts_with('.') || directory.starts_with('_') {
                    continue;
                }
                if let Some(skill) = skill_from_directory(&path) {
                    skills.push(SkillAdminRecord {
                        name: skill.name,
                        description: skill.description,
                        source: "user".to_owned(),
                        tags: skill.tags,
                        tools: skill.tools,
                        version: skill.version,
                        body: skill.body,
                    });
                }
            }
        }
        skills.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(skills)
    }
}

#[async_trait]
impl SkillAdmin for CliSkillAdmin {
    async fn list(&self, source: Option<&str>) -> Result<SkillListResult, String> {
        Ok(SkillListResult {
            skills: self.list_records(source)?,
        })
    }

    async fn create(
        &self,
        actor: &str,
        params: &SkillCreateParams,
    ) -> Result<SkillCreateResult, String> {
        validate_skill_name(&params.name)?;
        if !matches!(params.scope.as_str(), "global" | "user") {
            return Err("scope must be 'global' or 'user'".to_owned());
        }
        if params.body.trim().is_empty() {
            return Err("skill body must not be empty".to_owned());
        }
        let _guard = self
            .lock
            .lock()
            .map_err(|_| "skills lock poisoned".to_owned())?;
        let directory = self.root.join(&params.name);
        if directory.exists() {
            self.audit(actor, "create", &params.name, "duplicate");
            return Err(format!("skill {:?} already exists", params.name));
        }
        let content = build_skill_md(params);
        std::fs::create_dir_all(&directory)
            .map_err(|error| format!("skill directory failed: {error}"))?;
        std::fs::write(directory.join("SKILL.md"), content)
            .map_err(|error| format!("skill write failed: {error}"))?;
        self.audit(actor, "create", &params.name, "ok");
        Ok(SkillCreateResult {
            name: params.name.clone(),
            path: directory.to_string_lossy().into_owned(),
            // The Rust store is a single directory; scope is normalized to `user`.
            scope: "user".to_owned(),
        })
    }

    async fn delete(&self, actor: &str, name: &str) -> Result<(), String> {
        validate_skill_name(name)?;
        let _guard = self
            .lock
            .lock()
            .map_err(|_| "skills lock poisoned".to_owned())?;
        let directory = self.root.join(name);
        if !directory.is_dir() {
            self.audit(actor, "delete", name, "not_found");
            return Err(format!("skill {name:?} not found"));
        }
        std::fs::remove_dir_all(&directory)
            .map_err(|error| format!("skill delete failed: {error}"))?;
        self.audit(actor, "delete", name, "ok");
        Ok(())
    }
}

/// Python `^[a-z0-9]+(?:-[a-z0-9]+)*$` with a 64-char bound.
fn validate_skill_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 64 {
        return Err(format!("invalid skill name {name:?}"));
    }
    let mut previous_separator = true;
    for byte in name.bytes() {
        match byte {
            b'a'..=b'z' | b'0'..=b'9' => previous_separator = false,
            b'-' if !previous_separator => previous_separator = true,
            _ => return Err(format!("invalid skill name {name:?}")),
        }
    }
    if previous_separator {
        return Err(format!("invalid skill name {name:?}"));
    }
    Ok(())
}

/// Build the `SKILL.md` document exactly like the Python create endpoint.
fn build_skill_md(params: &SkillCreateParams) -> String {
    let mut lines = vec!["---".to_owned(), format!("name: {}", params.name)];
    if !params.description.is_empty() {
        lines.push(format!("description: {}", params.description));
    }
    lines.push("version: \"1.0\"".to_owned());
    if !params.tags.is_empty() {
        lines.push("tags:".to_owned());
        for tag in &params.tags {
            lines.push(format!("  - {tag}"));
        }
    }
    if !params.tools.is_empty() {
        lines.push("tools:".to_owned());
        for tool in &params.tools {
            lines.push(format!("  - {tool}"));
        }
    }
    lines.push("---".to_owned());
    lines.push(String::new());
    lines.push(params.body.clone());
    lines.push(String::new());
    lines.join("\n")
}

fn skill_from_directory(directory: &Path) -> Option<SkillFile> {
    let skill_md = directory.join("SKILL.md");
    if !skill_md.is_file() {
        return None;
    }
    let text = std::fs::read_to_string(&skill_md).ok()?;
    let (metadata, body) = parse_skill_md(&text);
    let directory_name = directory.file_name()?.to_string_lossy().into_owned();
    let name = metadata
        .get("name")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or(directory_name);
    let description = metadata
        .get("description")
        .map(value_to_string)
        .unwrap_or_default();
    let version = metadata
        .get("version")
        .map(value_to_string)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "1.0".to_owned());
    Some(SkillFile {
        name,
        description,
        tags: string_list(metadata.get("tags")),
        tools: string_list(metadata.get("tools")),
        version,
        body,
    })
}

/// Permissive SKILL.md parser matching `skills/models.py::parse_skill_md`: an
/// optional `---` front-matter block with `key: value`, `- item` lists and a
/// lightweight scalar parser; the rest is the instruction body.
fn parse_skill_md(text: &str) -> (BTreeMap<String, Value>, String) {
    let Some((yaml, body)) = split_front_matter(text) else {
        return (BTreeMap::new(), text.trim().to_owned());
    };
    let mut metadata: BTreeMap<String, Value> = BTreeMap::new();
    let mut current_key: Option<String> = None;
    for line in yaml.lines() {
        let stripped = line.trim();
        if stripped.is_empty() || stripped.starts_with('#') {
            continue;
        }
        if let Some(item) = stripped.strip_prefix("- ")
            && let Some(key) = &current_key
        {
            let entry = metadata
                .entry(key.clone())
                .or_insert_with(|| Value::Array(Vec::new()));
            if !entry.is_array() {
                *entry = Value::Array(Vec::new());
            }
            entry
                .as_array_mut()
                .expect("array")
                .push(parse_scalar(item.trim()));
            continue;
        }
        if let Some((key, value)) = stripped.split_once(':') {
            let key = key.trim().to_owned();
            let value = value.trim();
            current_key = Some(key.clone());
            if value.is_empty() {
                metadata.insert(key, Value::Null);
            } else {
                metadata.insert(key, parse_scalar(value));
            }
        }
    }
    (metadata, body.trim().to_owned())
}

fn split_front_matter(text: &str) -> Option<(&str, &str)> {
    let after = text.strip_prefix("---")?;
    let after = after.trim_start_matches([' ', '\t']);
    let after = after
        .strip_prefix("\r\n")
        .or_else(|| after.strip_prefix('\n'))?;
    let mut offset = 0usize;
    for line in after.split_inclusive('\n') {
        // Python's regex requires the closing fence to be newline-terminated, so
        // a file that ends with `---` and no trailing newline has no front matter.
        if !line.ends_with('\n') {
            break;
        }
        let without_newline = line.trim_end_matches(['\n', '\r']);
        if without_newline.trim_end_matches([' ', '\t']) == "---" {
            return Some((&after[..offset], &after[offset + line.len()..]));
        }
        offset += line.len();
    }
    None
}

fn parse_scalar(value: &str) -> Value {
    match value.to_ascii_lowercase().as_str() {
        "true" | "yes" => return Value::Bool(true),
        "false" | "no" => return Value::Bool(false),
        _ => {}
    }
    let bytes = value.as_bytes();
    if bytes.len() >= 2
        && ((bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\''))
    {
        return Value::String(value[1..value.len() - 1].to_owned());
    }
    if value.contains(',') {
        return Value::Array(
            value
                .split(',')
                .map(|part| Value::String(part.trim().trim_matches(['\'', '"']).to_owned()))
                .collect(),
        );
    }
    if let Ok(number) = value.parse::<i64>() {
        return Value::from(number);
    }
    Value::String(value.to_owned())
}

fn value_to_string(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Number(number) => number.to_string(),
        Value::Bool(flag) => flag.to_string(),
        _ => String::new(),
    }
}

fn string_list(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::String(text)) => vec![text.clone()],
        Some(Value::Array(items)) => items.iter().map(value_to_string).collect(),
        _ => Vec::new(),
    }
}

fn now_string() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default();
    cool_store::python_datetime(seconds, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_front_matter_lists_and_scalars() {
        let (metadata, body) = parse_skill_md(
            "---\nname: demo\ndescription: A demo\ntags:\n  - one\n  - two\nversion: \"2.0\"\n---\n\nDo the thing.\n",
        );
        assert_eq!(metadata.get("name").and_then(Value::as_str), Some("demo"));
        assert_eq!(
            metadata.get("description").and_then(Value::as_str),
            Some("A demo")
        );
        assert_eq!(string_list(metadata.get("tags")), vec!["one", "two"]);
        assert_eq!(metadata.get("version").and_then(Value::as_str), Some("2.0"));
        assert_eq!(body, "Do the thing.");
    }

    #[test]
    fn missing_front_matter_keeps_the_body() {
        let (metadata, body) = parse_skill_md("Just instructions.\n");
        assert!(metadata.is_empty());
        assert_eq!(body, "Just instructions.");
    }

    #[test]
    fn unterminated_closing_fence_is_not_front_matter() {
        // Python's regex requires a newline after the closing `---`.
        let text = "---\nname: x\n---";
        let (metadata, body) = parse_skill_md(text);
        assert!(metadata.is_empty());
        assert_eq!(body, text);
    }

    #[test]
    fn skill_name_validation_matches_python() {
        for valid in ["demo", "my-skill", "a1-b2"] {
            assert!(
                validate_skill_name(valid).is_ok(),
                "{valid} should be valid"
            );
        }
        for invalid in [
            "",
            "Upper",
            "trailing-",
            "-leading",
            "double--x",
            "a_b",
            "a/b",
            "..",
        ] {
            assert!(
                validate_skill_name(invalid).is_err(),
                "{invalid} should be invalid"
            );
        }
    }

    #[test]
    fn create_list_delete_round_trips() {
        let directory = tempfile::tempdir().unwrap();
        let admin = CliSkillAdmin::new(directory.path());
        let params = SkillCreateParams {
            idempotency_key: cool_protocol::IdempotencyKey::new(
                cool_app_server::client::new_idempotency_key("skill"),
            )
            .unwrap(),
            name: "demo".to_owned(),
            description: "A demo".to_owned(),
            tags: vec!["one".to_owned()],
            tools: vec!["read_file".to_owned()],
            body: "Do the thing.".to_owned(),
            scope: "user".to_owned(),
        };
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let created = admin.create("local-user", &params).await.unwrap();
            assert_eq!(created.name, "demo");
            assert_eq!(created.scope, "user");
            assert!(admin.create("local-user", &params).await.is_err());
            let listed = admin.list(None).await.unwrap();
            assert_eq!(listed.skills.len(), 1);
            assert_eq!(listed.skills[0].name, "demo");
            assert_eq!(listed.skills[0].tags, vec!["one"]);
            assert_eq!(listed.skills[0].body, "Do the thing.");
            // A non-`user`/`global` source filter yields nothing.
            assert!(admin.list(Some("plugin")).await.unwrap().skills.is_empty());
            // `global` is a single-store alias.
            assert_eq!(admin.list(Some("global")).await.unwrap().skills.len(), 1);
            admin.delete("local-user", "demo").await.unwrap();
            assert!(admin.delete("local-user", "demo").await.is_err());
            assert!(admin.list(None).await.unwrap().skills.is_empty());
        });
        assert!(directory.path().join(AUDIT_FILE).exists());
    }
}
