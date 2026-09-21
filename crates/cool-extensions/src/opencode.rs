//! OpenCode compatibility worker launcher (M11 §6.7).
//!
//! Executable OpenCode plugins are hosted only in an isolated Bun process that
//! speaks the versioned worker RPC. The launcher is deliberately data-oriented:
//! it validates the immutable install tree, keeps the writable data root
//! separate, maps the plugin's required capabilities onto a narrowing child
//! policy, and produces a [`WorkerLaunchSpec`] that grants only the plugin
//! roots, the granted workspace paths and a sanitized environment.
//!
//! The trusted core never embeds Bun/Node/npm APIs: it only knows how to spawn
//! the configured Bun executable with one entry file and a fixed argument
//! vector. Absence of Bun or of an executable plugin must leave the base
//! install untouched, so nothing here runs unless a caller explicitly builds a
//! spec.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

use cool_security::{Capability, CapabilityPolicy, Decision};

use crate::WorkerLaunchSpec;
use crate::loader::is_link_like;

/// Environment variable carrying the JSON array of granted workspace paths.
pub const GRANTED_WORKSPACES_ENV: &str = "COOL_GRANTED_WORKSPACES";
/// Environment variable naming the OpenCode plugin entry file.
pub const PLUGIN_ENTRY_ENV: &str = "COOL_OPENCODE_ENTRY";
/// Environment variable carrying the plugin's immutable install root.
pub const PLUGIN_ROOT_ENV: &str = "PLUGIN_ROOT";
/// Environment variable carrying the plugin's writable data root.
pub const PLUGIN_DATA_ENV: &str = "PLUGIN_DATA";
/// Environment variable pinning Bun's install/cache root to the data root.
pub const BUN_INSTALL_ENV: &str = "BUN_INSTALL";

/// File extensions treated as executable OpenCode plugin entries.
const ENTRY_EXTENSIONS: &[&str] = &["js", "mjs", "cjs", "ts", "mts", "cts", "jsx", "tsx"];

/// A reserved environment name a caller may not supply (it is set by the
/// launcher so a plugin cannot spoof its roots or grant itself workspaces), or
/// an interpreter/runtime injection vector.
const RESERVED_ENV: &[&str] = &[
    PLUGIN_ROOT_ENV,
    PLUGIN_DATA_ENV,
    PLUGIN_ENTRY_ENV,
    GRANTED_WORKSPACES_ENV,
    BUN_INSTALL_ENV,
    "PATH",
    "PATHEXT",
    "COMSPEC",
    "SYSTEMROOT",
    "WINDIR",
    "TEMP",
    "TMP",
    "TMPDIR",
    "NODE_OPTIONS",
    "NODE_PATH",
    "BUN_OPTIONS",
    "BUN_INSTALL_CACHE_DIR",
    "LD_PRELOAD",
    "LD_LIBRARY_PATH",
    "DYLD_INSERT_LIBRARIES",
    "DYLD_LIBRARY_PATH",
];

#[derive(Debug, PartialEq, Eq)]
pub enum OpenCodeSpecError {
    /// The install or data root is missing or link-like.
    InvalidRoot,
    /// The entry file is missing, escapes the install root, is link-like or is
    /// not an executable plugin file.
    InvalidEntry,
    /// A granted workspace is missing, link-like or overlaps the immutable
    /// install root.
    InvalidWorkspace,
    /// A caller-supplied environment name is reserved by the launcher.
    ReservedEnvironment(String),
    /// The core policy denies a capability the plugin requires.
    CapabilityDenied(Capability),
}

impl fmt::Display for OpenCodeSpecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRoot => formatter.write_str("OpenCode plugin roots are invalid or linked"),
            Self::InvalidEntry => {
                formatter.write_str("OpenCode plugin entry is missing, linked or non-executable")
            }
            Self::InvalidWorkspace => formatter
                .write_str("OpenCode granted workspace is invalid or overlaps the install root"),
            Self::ReservedEnvironment(name) => {
                write!(formatter, "OpenCode plugin environment {name} is reserved")
            }
            Self::CapabilityDenied(capability) => write!(
                formatter,
                "core policy denies the required capability {capability:?}"
            ),
        }
    }
}

impl std::error::Error for OpenCodeSpecError {}

/// Inputs needed to launch one OpenCode plugin worker. All paths are validated
/// before a spec is produced.
#[derive(Clone, Debug)]
pub struct OpenCodeWorkerConfig {
    /// The Bun executable. A bare name is resolved through `PATH` at spawn time.
    pub bun: PathBuf,
    /// Immutable installation root (content-addressed; never written).
    pub plugin_root: PathBuf,
    /// Writable plugin data root, kept outside the installation root.
    pub plugin_data: PathBuf,
    /// Executable entry file, inside the installation root.
    pub entry: PathBuf,
    /// Workspace paths explicitly granted to the worker.
    pub granted_workspaces: Vec<PathBuf>,
    /// Extra non-secret environment values (reserved names are rejected).
    pub environment: BTreeMap<String, String>,
    /// Capabilities the plugin declares it needs; undeclared ones stay denied.
    pub required_capabilities: BTreeSet<Capability>,
}

/// Child policy for an OpenCode worker: the core policy narrowed by the
/// plugin's declared capabilities. Undeclared capabilities are denied even when
/// the core allows them; the worker can never widen the core policy.
pub fn opencode_worker_policy(
    core: &CapabilityPolicy,
    required: &BTreeSet<Capability>,
) -> CapabilityPolicy {
    crate::narrowed_plugin_policy(
        core,
        required,
        &CapabilityPolicy::new(Some(Decision::Allow)),
    )
}

/// Validates an OpenCode plugin tree and produces an isolated [`WorkerLaunchSpec`].
///
/// The worker receives:
/// - the Bun program with a fixed `run <entry>` argument vector;
/// - a working directory inside the (writable) data root, never the install
///   root;
/// - `PLUGIN_ROOT`/`PLUGIN_DATA`/`COOL_OPENCODE_ENTRY`/`COOL_GRANTED_WORKSPACES`
///   and a `BUN_INSTALL` under the data root;
/// - a sanitized environment with no explicitly allowed secrets.
///
/// A capability the core policy denies fails closed instead of spawning.
pub fn opencode_launch_spec(
    config: OpenCodeWorkerConfig,
    core: &CapabilityPolicy,
) -> Result<WorkerLaunchSpec, OpenCodeSpecError> {
    for capability in &config.required_capabilities {
        if core.resolve(*capability) == Decision::Deny {
            return Err(OpenCodeSpecError::CapabilityDenied(*capability));
        }
    }
    let root = validated_root(&config.plugin_root)?;
    let data = writable_data_root(&config.plugin_data, &root)?;
    let entry = validated_entry(&root, &config.entry)?;
    let granted = validated_workspaces(&config.granted_workspaces, &root)?;

    let mut environment = BTreeMap::new();
    for (name, value) in &config.environment {
        if RESERVED_ENV
            .iter()
            .any(|reserved| name.eq_ignore_ascii_case(reserved))
        {
            return Err(OpenCodeSpecError::ReservedEnvironment(name.clone()));
        }
        environment.insert(name.clone(), value.clone());
    }
    let workspaces = granted
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    environment.insert(
        PLUGIN_ROOT_ENV.to_owned(),
        root.to_string_lossy().into_owned(),
    );
    environment.insert(
        PLUGIN_DATA_ENV.to_owned(),
        data.to_string_lossy().into_owned(),
    );
    environment.insert(
        PLUGIN_ENTRY_ENV.to_owned(),
        entry.to_string_lossy().into_owned(),
    );
    environment.insert(
        GRANTED_WORKSPACES_ENV.to_owned(),
        serde_json::to_string(&workspaces).expect("workspace list serializes"),
    );
    environment.insert(
        BUN_INSTALL_ENV.to_owned(),
        data.join("bun").to_string_lossy().into_owned(),
    );

    Ok(WorkerLaunchSpec {
        program: config.bun,
        args: vec!["run".to_owned(), entry.to_string_lossy().into_owned()],
        cwd: data,
        environment,
        // No secret is explicitly allowed through to an untrusted plugin.
        allowed_secret_environment: BTreeSet::new(),
    })
}

fn validated_root(path: &Path) -> Result<PathBuf, OpenCodeSpecError> {
    if !path.is_dir()
        || is_link_like(
            &std::fs::symlink_metadata(path).map_err(|_| OpenCodeSpecError::InvalidRoot)?,
        )
    {
        return Err(OpenCodeSpecError::InvalidRoot);
    }
    path.canonicalize()
        .map_err(|_| OpenCodeSpecError::InvalidRoot)
}

/// The writable plugin data root is created on demand (it does not carry
/// install-time integrity), but only after rejecting a path that is inside, or
/// contains, the immutable install root — so a rejected config never writes into
/// the install tree.
fn writable_data_root(path: &Path, root: &Path) -> Result<PathBuf, OpenCodeSpecError> {
    if path.starts_with(root) {
        return Err(OpenCodeSpecError::InvalidRoot);
    }
    // Resolve links in the deepest existing ancestor before creating anything,
    // so a symlinked parent cannot smuggle the data root into the install tree.
    let mut probe = path;
    while !probe.exists() {
        probe = probe.parent().ok_or(OpenCodeSpecError::InvalidRoot)?;
    }
    let ancestor = probe
        .canonicalize()
        .map_err(|_| OpenCodeSpecError::InvalidRoot)?;
    // Only an ancestor *inside* the install root is dangerous (it would make the
    // data root land in the install tree); an ancestor that merely contains the
    // install root is fine.
    if ancestor.starts_with(root) {
        return Err(OpenCodeSpecError::InvalidRoot);
    }
    std::fs::create_dir_all(path).map_err(|_| OpenCodeSpecError::InvalidRoot)?;
    let canonical = root_check(
        path.canonicalize()
            .map_err(|_| OpenCodeSpecError::InvalidRoot)?,
        root,
    )?;
    Ok(canonical)
}

/// Rejects a canonical directory that overlaps the install root either way.
fn root_check(path: PathBuf, root: &Path) -> Result<PathBuf, OpenCodeSpecError> {
    if path.starts_with(root) || root.starts_with(&path) {
        return Err(OpenCodeSpecError::InvalidRoot);
    }
    Ok(path)
}

fn validated_entry(root: &Path, entry: &Path) -> Result<PathBuf, OpenCodeSpecError> {
    let canonical = entry
        .canonicalize()
        .map_err(|_| OpenCodeSpecError::InvalidEntry)?;
    if !canonical.is_file()
        || !canonical.starts_with(root)
        || is_link_like(
            &std::fs::symlink_metadata(entry).map_err(|_| OpenCodeSpecError::InvalidEntry)?,
        )
    {
        return Err(OpenCodeSpecError::InvalidEntry);
    }
    let executable = canonical
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|extension| {
            ENTRY_EXTENSIONS
                .iter()
                .any(|allowed| extension.eq_ignore_ascii_case(allowed))
        });
    if !executable {
        return Err(OpenCodeSpecError::InvalidEntry);
    }
    Ok(canonical)
}

fn validated_workspaces(paths: &[PathBuf], root: &Path) -> Result<Vec<PathBuf>, OpenCodeSpecError> {
    let mut granted = BTreeSet::new();
    for path in paths {
        if !path.is_dir()
            || is_link_like(
                &std::fs::symlink_metadata(path)
                    .map_err(|_| OpenCodeSpecError::InvalidWorkspace)?,
            )
        {
            return Err(OpenCodeSpecError::InvalidWorkspace);
        }
        let canonical = path
            .canonicalize()
            .map_err(|_| OpenCodeSpecError::InvalidWorkspace)?;
        // The immutable install tree is never a writable workspace.
        if canonical.starts_with(root) || root.starts_with(&canonical) {
            return Err(OpenCodeSpecError::InvalidWorkspace);
        }
        granted.insert(canonical);
    }
    Ok(granted.into_iter().collect())
}
