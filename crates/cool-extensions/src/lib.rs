//! M8 extension boundary for declarative plugins, MCP, hooks and compatibility workers.
//!
//! Plugin-controlled executable code is always launched out of process. The trusted core parses
//! declarations, narrows policy and verifies review hashes before dispatching any side effect.

mod hooks;
mod loader;
mod mcp;
mod opencode;
mod runtime;
mod signing;
mod store;
mod worker;

pub use hooks::{HookAudit, HookEngine, HookError, HookInvocation, HookReviewStore};
pub use loader::{
    CompatibilityKind, Diagnostic, DiagnosticLevel, HookDeclaration, HookHandler, McpServer,
    PluginBundle, PluginLoader, PluginManifest, Skill,
};
pub use mcp::{
    McpClient, McpError, McpTool, McpToolPolicy, discover_plugin_tools,
    discover_plugin_tools_with_policy,
};
pub use opencode::{
    BUN_INSTALL_ENV, GRANTED_WORKSPACES_ENV, OpenCodeSpecError, OpenCodeWorkerConfig,
    PLUGIN_DATA_ENV, PLUGIN_ENTRY_ENV, PLUGIN_ROOT_ENV, opencode_launch_spec,
    opencode_worker_policy,
};
pub use runtime::ExtensionRuntime;
pub use signing::{
    KEYRING_FILE, SIGNATURE_FILE, SignatureStatus, TRANSPARENCY_FILE, TransparencyEntry,
    VerifiedSignature, append_transparency, inspect_signature, load_keyring, public_key_base64,
    read_transparency_log, sign_payload, signature_payload, verify_transparency_log,
};
pub use store::{InstalledPlugin, PluginStore, StoreError};
pub use worker::{
    CompatibilityAdapter, CompatibilityWorkerSupervisor, WorkerError, WorkerLaunchSpec,
    WorkerOperationClass, WorkerProtocol, WorkerRequestOutcome, WorkerResponse, WorkerRpcError,
};

pub fn plugin_status_event(
    plugin_id: impl Into<String>,
    status: impl Into<String>,
    code: Option<String>,
) -> cool_protocol::CanonicalEvent {
    cool_protocol::CanonicalEvent::PluginStatus(cool_protocol::PluginStatusEvent {
        plugin_id: plugin_id.into(),
        status: status.into(),
        code,
    })
}

use std::collections::{BTreeMap, BTreeSet};

use cool_security::{Capability, CapabilityPolicy, Decision};

pub fn capability(name: &str) -> Option<Capability> {
    match name {
        "read" => Some(Capability::Read),
        "write" => Some(Capability::Write),
        "execute" => Some(Capability::Execute),
        "network" => Some(Capability::Network),
        "git" => Some(Capability::Git),
        "send_external" => Some(Capability::SendExternal),
        _ => None,
    }
}

/// Builds a child policy that is incapable of widening either the core policy or the component's
/// declared capability set. Undeclared capabilities are denied even when the core allows them.
pub fn narrowed_plugin_policy(
    core: &CapabilityPolicy,
    declared: &BTreeSet<Capability>,
    requested: &CapabilityPolicy,
) -> CapabilityPolicy {
    let mut declaration = CapabilityPolicy::new(Some(Decision::Deny));
    for item in declared {
        declaration.set(*item, Decision::Allow);
    }
    core.narrow_with(&declaration).narrow_with(requested)
}

/// Minimal environment needed to launch a child process. Extension children never inherit the
/// host environment wholesale: plugin-owned values are added separately after secret filtering.
pub(crate) fn execution_environment_baseline() -> BTreeMap<String, String> {
    const NAMES: &[&str] = if cfg!(windows) {
        &[
            "PATH",
            "PATHEXT",
            "SystemRoot",
            "WINDIR",
            "ComSpec",
            "TEMP",
            "TMP",
        ]
    } else {
        &["PATH", "TMPDIR"]
    };
    NAMES
        .iter()
        .filter_map(|name| {
            std::env::var(name)
                .ok()
                .map(|value| ((*name).to_owned(), value))
        })
        .collect()
}
