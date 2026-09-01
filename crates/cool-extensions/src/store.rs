use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

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
