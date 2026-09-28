//! Process launchers (P0.3 / P2.17).
//!
//! Every tool that runs a host process goes through [`ProcessLauncher`] — the
//! trait is the single gate between the agent's tool calls and OS process
//! creation. The default is [`DisabledLauncher`], which fails closed; the
//! operator opts in per process via `COOL_PROCESS_LAUNCHER`, a profile
//! `settings["process_launcher"]` value, or a CLI flag (`--allow-shell`,
//! `--process-launcher=…`, `--sandbox=…`).
//!
//! - [`HostLauncher`] keeps the existing trusted-host containment: sanitized
//!   `env_clear` environment, `KillOnDrop`, and a killable containment unit
//!   (Windows Job Object / Unix process group). It cannot isolate the
//!   filesystem or network, so a `LaunchSpec` asking for `NetAccess` below
//!   `Full` fails closed.
//! - [`SandboxedLauncher`] wraps argv in an OS sandbox backend: `bwrap` on
//!   Linux, `sandbox-exec` (seatbelt) on macOS, `jobobject` on Windows
//!   (containment only — no FS/Net isolation in v1, see module docs on
//!   [`SandboxBackend`]).

use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use cool_security::RuleState;
use process_wrap::tokio::ChildWrapper;

use crate::tools::ToolError;

/// Which launcher implementation backs a `ToolContext`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LauncherKind {
    Disabled,
    Host,
    Sandboxed,
}

/// Network access requested for one launch. `Host` cannot isolate the
/// network; `SandboxedLauncher` enforces `None` via `--unshare-net` (bwrap) or
/// by omitting the network clause (seatbelt). `Pinned` is pragmatic v1: the
/// sandbox keeps shared networking and exports the pin list as
/// `COOL_NET_PINNED` for observability; a real allowlist proxy is a follow-up.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum NetAccess {
    None,
    Pinned(Vec<String>),
    #[default]
    Full,
}

/// Per-launch resource bounds; the capture layer in `tools.rs` enforces them.
#[derive(Clone, Debug)]
pub struct ResourceLimits {
    pub timeout: Duration,
    pub max_output_bytes: usize,
}

/// Everything a launcher needs to spawn one bounded process.
#[derive(Clone, Debug)]
pub struct LaunchSpec {
    pub cwd: PathBuf,
    /// Sanitized environment pairs — applied under `env_clear`.
    pub env: Vec<(String, String)>,
    /// Bytes piped to the child's stdin, when the caller streams input.
    pub stdin: Option<Vec<u8>>,
    pub net: NetAccess,
    pub limits: ResourceLimits,
}

pub trait ProcessLauncher: Send + Sync + fmt::Debug {
    fn spawn(
        &self,
        program: &str,
        args: &[String],
        spec: &LaunchSpec,
    ) -> Result<Box<dyn ChildWrapper>, ToolError>;

    fn kind(&self) -> LauncherKind;
}

/// Fail-closed default: no process may be spawned.
#[derive(Debug)]
pub struct DisabledLauncher;

impl ProcessLauncher for DisabledLauncher {
    fn spawn(
        &self,
        _program: &str,
        _args: &[String],
        _spec: &LaunchSpec,
    ) -> Result<Box<dyn ChildWrapper>, ToolError> {
        Err(ToolError::Security(
            "process launcher is disabled; run with --allow-shell or enable a launcher".to_owned(),
        ))
    }

    fn kind(&self) -> LauncherKind {
        LauncherKind::Disabled
    }
}

/// Trusted-host launcher: the child runs directly on the host inside a
/// killable containment unit (Job Object on Windows, process group on Unix)
/// with an `env_clear` sanitized environment. Network/filesystem isolation
/// beyond that is not possible — anything below `NetAccess::Full` fails closed.
#[derive(Debug)]
pub struct HostLauncher;

impl ProcessLauncher for HostLauncher {
    fn spawn(
        &self,
        program: &str,
        args: &[String],
        spec: &LaunchSpec,
    ) -> Result<Box<dyn ChildWrapper>, ToolError> {
        if spec.net != NetAccess::Full {
            return Err(ToolError::Security(
                "host launcher cannot isolate network access".to_owned(),
            ));
        }
        spawn_wrapped(
            &[program.to_owned()]
                .into_iter()
                .chain(args.iter().cloned())
                .collect::<Vec<_>>(),
            spec,
        )
    }

    fn kind(&self) -> LauncherKind {
        LauncherKind::Host
    }
}

/// OS sandbox backends for [`SandboxedLauncher`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SandboxBackend {
    /// Linux `bwrap` (bubblewrap): workspace bound read-write over a read-only
    /// root, `--unshare-net` on `NetAccess::None`.
    Bwrap,
    /// macOS `sandbox-exec` seatbelt profile: deny-by-default, workspace
    /// write-only subtree, optional network clause.
    Seatbelt,
    /// Windows Job Object containment only — **no** filesystem or network
    /// isolation in v1 (documented limitation).
    JobObject,
}

impl SandboxBackend {
    pub fn name(self) -> &'static str {
        match self {
            Self::Bwrap => "bwrap",
            Self::Seatbelt => "seatbelt",
            Self::JobObject => "jobobject",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "bwrap" => Some(Self::Bwrap),
            "seatbelt" | "sandbox-exec" | "sandbox_exec" => Some(Self::Seatbelt),
            "jobobject" | "job_object" | "job" => Some(Self::JobObject),
            _ => None,
        }
    }

    /// Whether the backend binary exists and can run on this host.
    pub fn available(self) -> bool {
        match self {
            Self::Bwrap => cfg!(target_os = "linux") && which_exists("bwrap"),
            Self::Seatbelt => {
                cfg!(target_os = "macos") && std::path::Path::new("/usr/bin/sandbox-exec").exists()
            }
            // Job Objects are a kernel feature — always available on Windows.
            Self::JobObject => cfg!(windows),
        }
    }

    /// The backend the current OS would auto-select, when any is available.
    pub fn detect() -> Option<Self> {
        [Self::Bwrap, Self::Seatbelt, Self::JobObject]
            .into_iter()
            .find(|backend| backend.available())
    }
}

fn which_exists(program: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths).any(|dir| {
                #[cfg(windows)]
                let candidate = dir.join(format!("{program}.exe"));
                #[cfg(not(windows))]
                let candidate = dir.join(program);
                candidate.is_file()
            })
        })
        .unwrap_or(false)
}

/// Availability of each sandbox backend on this host, for `cool doctor`.
pub fn sandbox_backend_status() -> Vec<serde_json::Value> {
    [
        SandboxBackend::Bwrap,
        SandboxBackend::Seatbelt,
        SandboxBackend::JobObject,
    ]
    .into_iter()
    .map(|backend| {
        serde_json::json!({
            "backend": backend.name(),
            "available": backend.available(),
        })
    })
    .collect()
}

/// Sandboxed process launcher (P2.17). Wraps argv in the OS sandbox before
/// delegating to the same spawn path as [`HostLauncher`], so containment and
/// kill semantics are unchanged. Missing backend binary fails closed.
#[derive(Debug)]
pub struct SandboxedLauncher {
    backend: SandboxBackend,
}

impl SandboxedLauncher {
    pub fn new(backend: SandboxBackend) -> Result<Self, String> {
        if !backend.available() {
            return Err(format!(
                "sandbox backend '{}' is not available on this host",
                backend.name()
            ));
        }
        Ok(Self { backend })
    }

    /// Auto-select an available backend for the current OS.
    pub fn detect() -> Result<Self, String> {
        match SandboxBackend::detect() {
            Some(backend) => Self::new(backend),
            None => Err("no sandbox backend is available on this host".to_owned()),
        }
    }

    pub fn backend(&self) -> SandboxBackend {
        self.backend
    }
}

impl ProcessLauncher for SandboxedLauncher {
    fn spawn(
        &self,
        program: &str,
        args: &[String],
        spec: &LaunchSpec,
    ) -> Result<Box<dyn ChildWrapper>, ToolError> {
        let argv = match self.backend {
            SandboxBackend::Bwrap => bwrap_argv(program, args, spec),
            SandboxBackend::Seatbelt => seatbelt_argv(program, args, spec),
            SandboxBackend::JobObject => {
                // v1: Job Object containment only (no FS/Net isolation).
                return HostLauncher.spawn(program, args, spec);
            }
        };
        let mut spec = spec.clone();
        if let NetAccess::Pinned(domains) = &spec.net {
            // Pragmatic v1: no allowlist proxy yet — the pin list is exported
            // for observability and the sandbox keeps shared networking.
            spec.env
                .push(("COOL_NET_PINNED".to_owned(), domains.join(",")));
        }
        spawn_wrapped(&argv, &spec)
    }

    fn kind(&self) -> LauncherKind {
        LauncherKind::Sandboxed
    }
}

/// `bwrap` argv: read-only root, workspace bound read-write, private /dev,
/// /proc, /tmp; `--unshare-net` when the spec denies network access.
pub fn bwrap_argv(program: &str, args: &[String], spec: &LaunchSpec) -> Vec<String> {
    let workspace = spec.cwd.to_string_lossy().into_owned();
    let mut argv = vec![
        "bwrap".to_owned(),
        "--die-with-parent".to_owned(),
        "--ro-bind".to_owned(),
        "/".to_owned(),
        "/".to_owned(),
        "--dev".to_owned(),
        "/dev".to_owned(),
        "--proc".to_owned(),
        "/proc".to_owned(),
        "--tmpfs".to_owned(),
        "/tmp".to_owned(),
        "--bind".to_owned(),
        workspace.clone(),
        workspace.clone(),
        "--chdir".to_owned(),
        workspace,
    ];
    if spec.net == NetAccess::None {
        argv.push("--unshare-net".to_owned());
    }
    argv.push("--".to_owned());
    argv.push(program.to_owned());
    argv.extend(args.iter().cloned());
    argv
}

/// `sandbox-exec` seatbelt argv: deny-by-default profile with read access to
/// the host filesystem, writes confined to the workspace (plus /private/tmp
/// for toolchain scratch), process exec/fork allowed, and a network clause
/// only when the spec allows network access.
pub fn seatbelt_argv(program: &str, args: &[String], spec: &LaunchSpec) -> Vec<String> {
    let workspace = spec.cwd.to_string_lossy().replace('\\', "/");
    let network = match spec.net {
        NetAccess::None => "",
        NetAccess::Full | NetAccess::Pinned(_) => "(allow network*)",
    };
    let profile = format!(
        "(version 1)(deny default)\
         (allow file-read* (subpath \"/\"))\
         (allow file-write* (subpath \"{workspace}\") (subpath \"/private/tmp\"))\
         (allow process-exec)(allow process-fork)(allow signal (target self))\
         {network}"
    );
    ["sandbox-exec", "-p", profile.as_str(), program]
        .into_iter()
        .map(str::to_owned)
        .chain(args.iter().cloned())
        .collect()
}

/// Shared spawn path: `env_clear` + sanitized env, piped stdio, killable
/// containment unit (Windows Job Object / Unix process group), KillOnDrop.
fn spawn_wrapped(argv: &[String], spec: &LaunchSpec) -> Result<Box<dyn ChildWrapper>, ToolError> {
    let Some((program, args)) = argv.split_first() else {
        return Err(ToolError::InvalidArguments(
            "empty argv for process spawn".to_owned(),
        ));
    };
    let mut command = process_wrap::tokio::CommandWrap::with_new(program, |command| {
        command
            .args(args)
            .current_dir(&spec.cwd)
            .env_clear()
            .envs(spec.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .stdin(if spec.stdin.is_some() {
                std::process::Stdio::piped()
            } else {
                std::process::Stdio::null()
            })
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
    });
    command.wrap(process_wrap::tokio::KillOnDrop);
    #[cfg(unix)]
    command.wrap(process_wrap::tokio::ProcessGroup::leader());
    #[cfg(windows)]
    command.wrap(process_wrap::tokio::JobObject);
    command.spawn().map_err(ToolError::Io)
}

/// The host-side context every executor injects into a `ToolContext` so the
/// main loop, the scheduler and subagents share one launcher, host
/// environment and live rule state.
#[derive(Clone)]
pub struct HostContext {
    pub launcher: Arc<dyn ProcessLauncher>,
    /// Host environment launched processes inherit (through `env_clear` +
    /// `sanitize_environment`); empty when no launcher is configured.
    pub environment: HashMap<String, String>,
    /// Shared project + session rule state (P1.6).
    pub rules: Arc<RuleState>,
}

impl fmt::Debug for HostContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HostContext")
            .field("launcher", &self.launcher)
            .field("environment", &self.environment.len())
            .finish_non_exhaustive()
    }
}

impl Default for HostContext {
    fn default() -> Self {
        Self {
            launcher: Arc::new(DisabledLauncher),
            environment: HashMap::new(),
            rules: Arc::new(RuleState::default()),
        }
    }
}

/// Resolve a launcher from the selection chain's final choice.
/// `kind` vocabulary: `disabled|none`, `host|trusted`, `sandboxed|sandbox`,
/// or a backend name (`bwrap`/`seatbelt`/`jobobject`) which implies sandboxed.
pub fn resolve_launcher(
    kind: &str,
    backend: Option<SandboxBackend>,
) -> Result<Arc<dyn ProcessLauncher>, String> {
    match kind {
        "" | "disabled" | "none" | "off" => Ok(Arc::new(DisabledLauncher)),
        "host" | "trusted" | "allow-shell" | "allow_shell" => Ok(Arc::new(HostLauncher)),
        "sandboxed" | "sandbox" | "auto" => {
            let launcher = match backend {
                Some(backend) => SandboxedLauncher::new(backend),
                None => SandboxedLauncher::detect(),
            }?;
            Ok(Arc::new(launcher))
        }
        other => match SandboxBackend::parse(other) {
            Some(backend) => Ok(Arc::new(SandboxedLauncher::new(backend)?)),
            None => Err(format!(
                "unknown process launcher '{other}' (expected disabled|host|sandboxed|bwrap|seatbelt|jobobject)"
            )),
        },
    }
}

/// The launcher selection chain (P0.3):
/// `COOL_PROCESS_LAUNCHER` env → profile `settings["process_launcher"]` →
/// explicit flag → default disabled. `COOL_SANDBOX_BACKEND` /
/// `settings["sandbox_backend"]` pick the backend when the kind is sandboxed.
pub fn launcher_from_env() -> Result<Option<Arc<dyn ProcessLauncher>>, String> {
    let kind = std::env::var("COOL_PROCESS_LAUNCHER").unwrap_or_default();
    if kind.is_empty() {
        return Ok(None);
    }
    let backend = std::env::var("COOL_SANDBOX_BACKEND")
        .ok()
        .filter(|value| !value.is_empty())
        .map(|value| {
            SandboxBackend::parse(&value)
                .ok_or_else(|| format!("unknown COOL_SANDBOX_BACKEND '{value}'"))
        })
        .transpose()?;
    resolve_launcher(&kind, backend).map(Some)
}

/// Build the profile-level override: `settings["process_launcher"]` (+ optional
/// `settings["sandbox_backend"]`). Missing key → `None` (chain continues).
pub fn launcher_from_profile(
    settings: Option<&serde_json::Value>,
) -> Result<Option<Arc<dyn ProcessLauncher>>, String> {
    let Some(settings) = settings.and_then(serde_json::Value::as_object) else {
        return Ok(None);
    };
    let kind = settings
        .get("process_launcher")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if kind.is_empty() {
        return Ok(None);
    }
    let backend = settings
        .get("sandbox_backend")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .map(|value| {
            SandboxBackend::parse(value).ok_or_else(|| format!("unknown sandbox_backend '{value}'"))
        })
        .transpose()?;
    resolve_launcher(kind, backend).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(net: NetAccess) -> LaunchSpec {
        LaunchSpec {
            cwd: PathBuf::from("/ws/project"),
            env: Vec::new(),
            stdin: None,
            net,
            limits: ResourceLimits {
                timeout: Duration::from_secs(5),
                max_output_bytes: 1024,
            },
        }
    }

    #[test]
    fn disabled_launcher_fails_closed() {
        let launcher = DisabledLauncher;
        assert_eq!(launcher.kind(), LauncherKind::Disabled);
        let error = launcher
            .spawn("echo", &["hi".to_owned()], &spec(NetAccess::Full))
            .unwrap_err();
        assert!(matches!(error, ToolError::Security(_)));
    }

    #[test]
    fn host_launcher_rejects_network_isolation() {
        let error = HostLauncher
            .spawn("echo", &[], &spec(NetAccess::None))
            .unwrap_err();
        assert!(matches!(error, ToolError::Security(_)));
    }

    #[test]
    fn bwrap_argv_binds_workspace_and_unshares_net_when_denied() {
        let argv = bwrap_argv("cargo", &["check".to_owned()], &spec(NetAccess::None));
        assert_eq!(argv.first().map(String::as_str), Some("bwrap"));
        let tail = argv.split(|item| item == "--").nth(1).unwrap();
        assert_eq!(tail, ["cargo", "check"]);
        assert!(argv.iter().any(|item| item == "--unshare-net"));
        let bind = argv
            .windows(2)
            .find(|pair| pair[0] == "--bind")
            .expect("workspace rw bind present");
        assert_eq!(bind[1], "/ws/project");
    }

    #[test]
    fn bwrap_argv_keeps_network_when_full() {
        let argv = bwrap_argv("echo", &[], &spec(NetAccess::Full));
        assert!(!argv.iter().any(|item| item == "--unshare-net"));
    }

    #[test]
    fn seatbelt_argv_denies_by_default_and_confines_writes() {
        let argv = seatbelt_argv("tsc", &["--noEmit".to_owned()], &spec(NetAccess::None));
        assert_eq!(argv.first().map(String::as_str), Some("sandbox-exec"));
        let profile = argv.get(2).expect("profile");
        assert!(profile.contains("(deny default)"));
        assert!(profile.contains("/ws/project"));
        assert!(!profile.contains("network"));
        let argv = seatbelt_argv("tsc", &[], &spec(NetAccess::Full));
        assert!(argv.get(2).unwrap().contains("(allow network*)"));
    }

    #[test]
    fn resolve_launcher_vocabulary_fails_closed_on_unknown() {
        assert_eq!(
            resolve_launcher("", None).unwrap().kind(),
            LauncherKind::Disabled
        );
        assert_eq!(
            resolve_launcher("host", None).unwrap().kind(),
            LauncherKind::Host
        );
        assert!(resolve_launcher("definitely-not-a-launcher", None).is_err());
        assert!(
            resolve_launcher("sandboxed", Some(SandboxBackend::Bwrap)).is_err()
                || cfg!(target_os = "linux")
        );
    }

    #[test]
    fn backend_parsing_and_status_report() {
        assert_eq!(SandboxBackend::parse("bwrap"), Some(SandboxBackend::Bwrap));
        assert_eq!(
            SandboxBackend::parse("sandbox-exec"),
            Some(SandboxBackend::Seatbelt)
        );
        assert_eq!(SandboxBackend::parse("bogus"), None);
        let status = sandbox_backend_status();
        assert_eq!(status.len(), 3);
        assert_eq!(
            status
                .iter()
                .map(|entry| entry["backend"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["bwrap", "seatbelt", "jobobject"]
        );
    }

    /// Real sandbox exec — only runs where the backend exists.
    #[cfg(windows)]
    #[tokio::test]
    async fn jobobject_sandbox_spawns_a_real_process() {
        let launcher = SandboxedLauncher::new(SandboxBackend::JobObject).unwrap();
        assert_eq!(launcher.kind(), LauncherKind::Sandboxed);
        let mut spec = spec(NetAccess::Full);
        spec.cwd = std::env::temp_dir();
        let mut child = launcher
            .spawn("cmd.exe", &["/c".to_owned(), "exit 0".to_owned()], &spec)
            .unwrap();
        let status = child.wait().await.unwrap();
        assert!(status.success());
    }
}
