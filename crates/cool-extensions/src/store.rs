use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{PluginBundle, PluginLoader};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct InstalledPlugin {
    pub name: String,
    pub version: String,
    pub enabled: bool,
    pub source_type: String,
    pub source: String,
    pub revision: String,
    pub content_hash: String,
    pub install_path: String,
    pub data_path: String,
    pub installed_at: String,
    #[serde(default)]
    pub diagnostics: Vec<BTreeMap<String, String>>,
    #[serde(default)]
    pub resolved_dependencies: Vec<String>,
    #[serde(default)]
    pub required_capabilities: Vec<String>,
}

#[derive(Debug)]
pub enum StoreError {
    Io(std::io::Error),
    Json(serde_json::Error),
    Invalid(String),
    Poisoned,
}

impl fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "plugin store I/O error: {error}"),
            Self::Json(error) => write!(formatter, "plugin store JSON error: {error}"),
            Self::Invalid(message) => write!(formatter, "plugin store is invalid: {message}"),
            Self::Poisoned => formatter.write_str("plugin store lock is poisoned"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<std::io::Error> for StoreError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_json::Error> for StoreError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LockDocument {
    lock_version: u32,
    plugins: BTreeMap<String, InstalledPlugin>,
}

#[derive(Clone)]
pub struct PluginStore {
    root: PathBuf,
    lock_path: PathBuf,
    write_lock: Arc<Mutex<()>>,
    loader: PluginLoader,
}

impl PluginStore {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        let root = root.canonicalize()?;
        Ok(Self {
            lock_path: root.join("plugins.lock.json"),
            root,
            write_lock: Arc::new(Mutex::new(())),
            loader: PluginLoader,
        })
    }

    /// Reads the Python M3 lockfile unchanged. Rust review state is stored beside it so the Python
    /// lifecycle can continue reading and writing its own portable document during migration.
    pub fn list(&self) -> Result<Vec<InstalledPlugin>, StoreError> {
        Ok(self.read()?.plugins.into_values().collect())
    }

    pub fn load_enabled(&self) -> Result<Vec<PluginBundle>, StoreError> {
        self.load_enabled_isolated()?.into_iter().collect()
    }

    pub fn load_enabled_isolated(
        &self,
    ) -> Result<Vec<Result<PluginBundle, StoreError>>, StoreError> {
        Ok(self
            .list()?
            .into_iter()
            .filter(|entry| entry.enabled)
            .map(|entry| self.load_entry(entry))
            .collect())
    }

    fn load_entry(&self, entry: InstalledPlugin) -> Result<PluginBundle, StoreError> {
        self.validate_paths(&entry)?;
        let bundle = self
            .loader
            .load(Path::new(&entry.install_path), Path::new(&entry.data_path))
            .map_err(|error| StoreError::Invalid(error.to_string()))?;
        if bundle.content_hash != entry.content_hash {
            return Err(StoreError::Invalid(format!(
                "content hash mismatch for {}",
                entry.name
            )));
        }
        if bundle
            .manifest
            .as_ref()
            .map(|manifest| manifest.name.as_str())
            != Some(entry.name.as_str())
        {
            return Err(StoreError::Invalid(format!(
                "manifest identity mismatch for {}",
                entry.name
            )));
        }
        Ok(bundle)
    }

    pub fn get(&self, name: &str) -> Result<Option<InstalledPlugin>, StoreError> {
        Ok(self.read()?.plugins.remove(name))
    }

    pub fn install_local(&self, source: &Path) -> Result<InstalledPlugin, StoreError> {
        self.install_directory(source, "local", &source.to_string_lossy(), "", None, false)
    }

    pub fn install_git(&self, source: &str, revision: &str) -> Result<InstalledPlugin, StoreError> {
        if revision.len() != 40 || !revision.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(StoreError::Invalid(
                "Git revision must be a full 40-character commit SHA".to_owned(),
            ));
        }
        if source.is_empty() || source.starts_with('-') || source.contains('\0') {
            return Err(StoreError::Invalid(
                "Git source is empty or looks like a command option".to_owned(),
            ));
        }
        let staging_root = std::env::temp_dir().join(format!("cool-git-{}", Uuid::new_v4()));
        fs::create_dir_all(&staging_root)?;
        let checkout = staging_root.join("checkout");
        let install = (|| -> Result<InstalledPlugin, StoreError> {
            run_git(&[
                "clone",
                "--no-checkout",
                source,
                &checkout.to_string_lossy(),
            ])?;
            if run_git(&[
                "-C",
                &checkout.to_string_lossy(),
                "checkout",
                "--detach",
                revision,
            ])
            .is_err()
            {
                run_git(&[
                    "-C",
                    &checkout.to_string_lossy(),
                    "fetch",
                    "--depth",
                    "1",
                    "origin",
                    revision,
                ])?;
                run_git(&[
                    "-C",
                    &checkout.to_string_lossy(),
                    "checkout",
                    "--detach",
                    revision,
                ])?;
            }
            let resolved = run_git(&["-C", &checkout.to_string_lossy(), "rev-parse", "HEAD"])?;
            let resolved = resolved.trim().to_ascii_lowercase();
            if resolved != revision.to_ascii_lowercase() {
                return Err(StoreError::Invalid(format!(
                    "Git checkout mismatch: expected {revision}, got {resolved}"
                )));
            }
            self.install_directory(&checkout, "git", source, &resolved, None, false)
        })();
        let _ = fs::remove_dir_all(&staging_root);
        install
    }

    pub fn set_enabled(&self, name: &str, enabled: bool) -> Result<InstalledPlugin, StoreError> {
        let _guard = self.write_lock.lock().map_err(|_| StoreError::Poisoned)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.root.join("rust-extensions.lock"))?;
        lock.lock()?;
        let mut document = self.read()?;
        let Some(entry) = document.plugins.get_mut(name) else {
            return Err(StoreError::Invalid(format!(
                "plugin is not installed: {name}"
            )));
        };
        if enabled {
            self.load_entry(entry.clone())?;
        }
        entry.enabled = enabled;
        let result = self.write(&document);
        lock.unlock()?;
        result?;
        Ok(document
            .plugins
            .remove(name)
            .expect("the entry was present before the write"))
    }

    fn install_directory(
        &self,
        source: &Path,
        source_type: &str,
        source_label: &str,
        revision: &str,
        expected_name: Option<&str>,
        replacing: bool,
    ) -> Result<InstalledPlugin, StoreError> {
        let _guard = self.write_lock.lock().map_err(|_| StoreError::Poisoned)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.root.join("rust-extensions.lock"))?;
        lock.lock()?;
        let result = self.install_directory_locked(
            source,
            source_type,
            source_label,
            revision,
            expected_name,
            replacing,
        );
        lock.unlock()?;
        result
    }

    fn install_directory_locked(
        &self,
        source: &Path,
        source_type: &str,
        source_label: &str,
        revision: &str,
        expected_name: Option<&str>,
        replacing: bool,
    ) -> Result<InstalledPlugin, StoreError> {
        if !source.is_dir() {
            return Err(StoreError::Invalid(format!(
                "plugin source is not a directory: {}",
                source.display()
            )));
        }
        // Link rejection must run on the caller-supplied path: canonicalizing a
        // symlink/junction first would resolve it and hide the link itself.
        self.loader
            .reject_links(source)
            .map_err(|error| StoreError::Invalid(error.to_string()))?;
        let canonical_source = source.canonicalize()?;
        if canonical_source.starts_with(&self.root) {
            return Err(StoreError::Invalid(
                "plugin source must not contain the plugin store".to_owned(),
            ));
        }
        self.loader
            .reject_links(&canonical_source)
            .map_err(|error| StoreError::Invalid(error.to_string()))?;
        let installations = self.root.join("installations");
        let data_root = self.root.join("data");
        fs::create_dir_all(&installations)?;
        fs::create_dir_all(&data_root)?;
        let staging = installations.join(format!("install-{}", Uuid::new_v4()));
        fs::create_dir_all(&staging)?;
        let result = (|| -> Result<InstalledPlugin, StoreError> {
            copy_tree(&canonical_source, &staging)?;
            let provisional_data = data_root.join("staging");
            let bundle = self
                .loader
                .load(&staging, &provisional_data)
                .map_err(|error| StoreError::Invalid(error.to_string()))?;
            let Some(manifest) = bundle.manifest.clone() else {
                return Err(StoreError::Invalid(
                    "plugin manifest is not loadable".to_owned(),
                ));
            };
            if !bundle.loadable() {
                let blockers = bundle
                    .diagnostics
                    .iter()
                    .filter(|diagnostic| {
                        matches!(diagnostic.level, crate::loader::DiagnosticLevel::Blocker)
                    })
                    .map(|diagnostic| diagnostic.message.clone())
                    .collect::<Vec<_>>();
                return Err(StoreError::Invalid(if blockers.is_empty() {
                    "plugin manifest is not loadable".to_owned()
                } else {
                    blockers.join("; ")
                }));
            }
            if let Some(expected) = expected_name
                && manifest.name != expected
            {
                return Err(StoreError::Invalid(format!(
                    "update identity mismatch: expected {expected}, got {}",
                    manifest.name
                )));
            }
            let mut document = self.read()?;
            if document.plugins.contains_key(&manifest.name) && !replacing {
                return Err(StoreError::Invalid(format!(
                    "plugin is already installed: {}; use update",
                    manifest.name
                )));
            }
            let content_hash = self
                .loader
                .content_hash(&staging)
                .map_err(|error| StoreError::Invalid(error.to_string()))?;
            let destination = installations.join(&manifest.name).join(&content_hash);
            fs::create_dir_all(
                destination
                    .parent()
                    .expect("content-addressed destination has a parent"),
            )?;
            if !destination.exists() {
                fs::rename(&staging, &destination)?;
            }
            let destination_hash = self
                .loader
                .content_hash(&destination)
                .map_err(|error| StoreError::Invalid(error.to_string()))?;
            if destination_hash != content_hash {
                return Err(StoreError::Invalid(format!(
                    "content-addressed installation is corrupted: {}",
                    manifest.name
                )));
            }
            let plugin_data = data_root.join(&manifest.name);
            fs::create_dir_all(&plugin_data)?;
            let entry = InstalledPlugin {
                name: manifest.name.clone(),
                version: manifest.version.clone(),
                enabled: false,
                source_type: source_type.to_owned(),
                source: source_label.to_owned(),
                revision: revision.to_owned(),
                content_hash: content_hash.clone(),
                install_path: destination.to_string_lossy().into_owned(),
                data_path: plugin_data.to_string_lossy().into_owned(),
                installed_at: timestamp(),
                diagnostics: bundle
                    .diagnostics
                    .iter()
                    .map(|diagnostic| {
                        let mut fields = BTreeMap::new();
                        fields.insert("code".to_owned(), diagnostic.code.clone());
                        fields.insert(
                            "level".to_owned(),
                            match diagnostic.level {
                                crate::loader::DiagnosticLevel::Info => "info",
                                crate::loader::DiagnosticLevel::Warning => "warning",
                                crate::loader::DiagnosticLevel::Error => "error",
                                crate::loader::DiagnosticLevel::Blocker => "blocker",
                            }
                            .to_owned(),
                        );
                        fields.insert("message".to_owned(), diagnostic.message.clone());
                        fields.insert("path".to_owned(), diagnostic.path.clone());
                        fields
                    })
                    .collect(),
                resolved_dependencies: resolved_dependencies(&manifest.name, &bundle),
                required_capabilities: required_capabilities(&bundle),
            };
            document.plugins.insert(entry.name.clone(), entry.clone());
            self.write(&document)?;
            Ok(entry)
        })();
        if staging.exists() {
            let _ = fs::remove_dir_all(&staging);
        }
        result
    }

    fn write(&self, document: &LockDocument) -> Result<(), StoreError> {
        let payload = serde_json::to_vec_pretty(document)?;
        let temporary = self
            .root
            .join(format!("plugins-lock.{}.tmp", Uuid::new_v4()));
        fs::write(&temporary, payload)?;
        replace_file(&temporary, &self.lock_path)
    }

    pub fn set_hook_review(
        &self,
        plugin: &str,
        hook: &str,
        trust_hash: &str,
    ) -> Result<(), StoreError> {
        let _guard = self.write_lock.lock().map_err(|_| StoreError::Poisoned)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.root.join("rust-extensions.lock"))?;
        lock.lock()?;
        if !self.read()?.plugins.contains_key(plugin) {
            return Err(StoreError::Invalid(format!(
                "plugin is not installed: {plugin}"
            )));
        }
        let mut reviews = self.read_reviews()?;
        reviews
            .entry(plugin.to_owned())
            .or_default()
            .insert(hook.to_owned(), trust_hash.to_owned());
        let result = self.write_reviews(&reviews);
        lock.unlock()?;
        result
    }

    pub fn reviewed_hook_hashes(
        &self,
        plugin: &str,
    ) -> Result<BTreeMap<String, String>, StoreError> {
        Ok(self.read_reviews()?.remove(plugin).unwrap_or_default())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn read_reviews(&self) -> Result<BTreeMap<String, BTreeMap<String, String>>, StoreError> {
        let path = self.root.join("hook-reviews.json");
        if !path.exists() {
            return Ok(BTreeMap::new());
        }
        Ok(serde_json::from_slice(&fs::read(path)?)?)
    }

    fn write_reviews(
        &self,
        reviews: &BTreeMap<String, BTreeMap<String, String>>,
    ) -> Result<(), StoreError> {
        let temporary = self
            .root
            .join(format!("hook-reviews.{}.tmp", std::process::id()));
        fs::write(&temporary, serde_json::to_vec_pretty(reviews)?)?;
        replace_file(&temporary, &self.root.join("hook-reviews.json"))
    }

    fn read(&self) -> Result<LockDocument, StoreError> {
        if !self.lock_path.exists() {
            return Ok(LockDocument {
                lock_version: 1,
                plugins: BTreeMap::new(),
            });
        }
        let document: LockDocument = serde_json::from_slice(&fs::read(&self.lock_path)?)?;
        if document.lock_version != 1 {
            return Err(StoreError::Invalid("unsupported lock version".to_owned()));
        }
        for (key, entry) in &document.plugins {
            if key != &entry.name {
                return Err(StoreError::Invalid("plugin key/name mismatch".to_owned()));
            }
            self.validate_paths(entry)?;
        }
        Ok(document)
    }

    fn validate_paths(&self, entry: &InstalledPlugin) -> Result<(), StoreError> {
        if !matches!(entry.source_type.as_str(), "local" | "git") {
            return Err(StoreError::Invalid(
                "plugin source type is invalid".to_owned(),
            ));
        }
        if entry.content_hash.len() != 64
            || !entry
                .content_hash
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(StoreError::Invalid(
                "plugin content hash is invalid".to_owned(),
            ));
        }
        let expected = [
            self.root
                .join("installations")
                .join(&entry.name)
                .join(&entry.content_hash),
            self.root.join("data").join(&entry.name),
        ];
        for (raw, expected) in [&entry.install_path, &entry.data_path]
            .into_iter()
            .zip(expected)
        {
            let path = Path::new(raw);
            if !path.is_absolute() {
                return Err(StoreError::Invalid(
                    "plugin paths must be absolute".to_owned(),
                ));
            }
            let canonical = path.canonicalize()?;
            if !canonical.starts_with(&self.root) {
                return Err(StoreError::Invalid("plugin path escapes store".to_owned()));
            }
            let expected = expected
                .canonicalize()
                .map_err(|_| StoreError::Invalid("plugin path binding is missing".to_owned()))?;
            if canonical != expected {
                return Err(StoreError::Invalid(
                    "plugin path does not match its canonical name/hash binding".to_owned(),
                ));
            }
        }
        Ok(())
    }
}

fn is_bare_command(command: &str) -> bool {
    !command.is_empty()
        && !Path::new(command).is_absolute()
        && !command.contains('/')
        && !command.contains('\\')
}

fn resolved_dependencies(plugin: &str, bundle: &crate::PluginBundle) -> Vec<String> {
    let mut dependencies = std::collections::BTreeSet::new();
    for server in &bundle.mcp_servers {
        if let crate::McpServer::Stdio { command, .. } = server {
            let command = command.to_string_lossy();
            if is_bare_command(&command) {
                dependencies.insert(command.into_owned());
            }
        }
    }
    for hook in &bundle.hooks {
        if let crate::HookHandler::Command { command, .. } = &hook.handler {
            let command = command.to_string_lossy();
            if is_bare_command(&command) {
                dependencies.insert(command.into_owned());
            }
        }
    }
    let _ = plugin;
    dependencies.into_iter().collect()
}

fn required_capabilities(bundle: &crate::PluginBundle) -> Vec<String> {
    let mut capabilities = std::collections::BTreeSet::new();
    for hook in &bundle.hooks {
        for capability in &hook.capabilities {
            capabilities.insert(capability.as_str().to_owned());
        }
    }
    for server in &bundle.mcp_servers {
        match server {
            crate::McpServer::Stdio { .. } => {
                capabilities.insert("execute".to_owned());
            }
            crate::McpServer::StreamableHttp { .. } => {
                capabilities.insert("network".to_owned());
            }
        }
    }
    capabilities.into_iter().collect()
}

fn copy_tree(source: &Path, destination: &Path) -> Result<(), StoreError> {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == ".git" || name == ".gitmodules" {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_symlink() {
            return Err(StoreError::Invalid(
                "plugin source contains a symlink".to_owned(),
            ));
        }
        let target = destination.join(&name);
        if metadata.is_dir() {
            fs::create_dir_all(&target)?;
            copy_tree(&entry.path(), &target)?;
        } else if metadata.is_file() {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

fn run_git(arguments: &[&str]) -> Result<String, StoreError> {
    // Git output is redirected to files instead of pipes: polling `try_wait`
    // with unread pipes can deadlock once a pipe buffer fills.
    let capture_root = std::env::temp_dir().join(format!("cool-git-io-{}", Uuid::new_v4()));
    fs::create_dir_all(&capture_root)?;
    let stdout_path = capture_root.join("stdout.log");
    let stderr_path = capture_root.join("stderr.log");
    let result = (|| -> Result<String, StoreError> {
        let stdout = std::fs::File::create(&stdout_path)?;
        let stderr = std::fs::File::create(&stderr_path)?;
        let mut child = std::process::Command::new("git")
            .args(arguments)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::from(stdout))
            .stderr(std::process::Stdio::from(stderr))
            .spawn()
            .map_err(StoreError::Io)?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {
                    if std::time::Instant::now() > deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(StoreError::Invalid(
                            "Git source operation timed out".to_owned(),
                        ));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                Err(error) => return Err(StoreError::Io(error)),
            }
        };
        let stdout = fs::read_to_string(&stdout_path).unwrap_or_default();
        if status.success() {
            return Ok(stdout);
        }
        let stderr = fs::read_to_string(&stderr_path).unwrap_or_default();
        Err(StoreError::Invalid(format!(
            "Git source operation failed: {}",
            stderr.trim()
        )))
    })();
    let _ = fs::remove_dir_all(&capture_root);
    result
}

fn timestamp() -> String {
    let elapsed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let seconds = elapsed.as_secs();
    let days = (seconds / 86_400) as i64;
    let seconds_of_day = seconds % 86_400;
    let (year, month, day) = civil_date(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        seconds_of_day / 3_600,
        (seconds_of_day % 3_600) / 60,
        seconds_of_day % 60,
        elapsed.subsec_millis()
    )
}

// Gregorian civil date from Unix epoch days (Howard Hinnant's public-domain algorithm).
fn civil_date(days_since_epoch: i64) -> (i64, u32, u32) {
    let days = days_since_epoch + 719_468;
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year, month as u32, day as u32)
}

fn replace_file(source: &Path, destination: &Path) -> Result<(), StoreError> {
    let backup = destination.with_extension("json.backup");
    if destination.exists() {
        if backup.exists() {
            fs::remove_file(&backup)?;
        }
        fs::rename(destination, &backup)?;
    }
    match fs::rename(source, destination) {
        Ok(()) => {
            if backup.exists() {
                fs::remove_file(backup)?;
            }
            Ok(())
        }
        Err(error) => {
            if backup.exists() {
                let _ = fs::rename(&backup, destination);
            }
            Err(StoreError::Io(error))
        }
    }
}
