//! M6 security kernel primitives.
//!
//! Decisions are pure and fail closed.  Execution code consumes these typed
//! results; workers never get to widen a core policy or bypass approval.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;

use base64::Engine as _;
use fernet::Fernet;
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::{Host, Url};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Decision {
    Allow,
    Ask,
    Deny,
}

impl Decision {
    pub fn stricter(self, other: Self) -> Self {
        self.max(other)
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Capability {
    Read,
    Write,
    Execute,
    Network,
    Git,
    SendExternal,
}

#[derive(Clone, Debug, Default)]
pub struct CapabilityPolicy {
    wildcard: Option<Decision>,
    decisions: BTreeMap<Capability, Decision>,
}

impl CapabilityPolicy {
    pub fn new(wildcard: Option<Decision>) -> Self {
        Self {
            wildcard,
            decisions: BTreeMap::new(),
        }
    }

    pub fn set(&mut self, capability: Capability, decision: Decision) {
        self.decisions.insert(capability, decision);
    }

    pub fn resolve(&self, capability: Capability) -> Decision {
        self.decisions
            .get(&capability)
            .copied()
            .or(self.wildcard)
            .unwrap_or(Decision::Allow)
    }

    pub fn evaluate(
        &self,
        required: impl IntoIterator<Item = Capability>,
        tool_decision: Decision,
    ) -> PolicyDecision {
        let required = required.into_iter().collect::<BTreeSet<_>>();
        let effective = required.iter().fold(tool_decision, |current, capability| {
            current.stricter(self.resolve(*capability))
        });
        PolicyDecision {
            required,
            effective,
        }
    }

    /// A child/plugin policy may narrow, but can never widen, the core policy.
    pub fn narrow_with(&self, child: &Self) -> Self {
        let mut result = Self::new(Some(
            self.wildcard
                .unwrap_or(Decision::Allow)
                .stricter(child.wildcard.unwrap_or(Decision::Allow)),
        ));
        for capability in [
            Capability::Read,
            Capability::Write,
            Capability::Execute,
            Capability::Network,
            Capability::Git,
            Capability::SendExternal,
        ] {
            result.set(
                capability,
                self.resolve(capability).stricter(child.resolve(capability)),
            );
        }
        result
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PolicyDecision {
    pub required: BTreeSet<Capability>,
    pub effective: Decision,
}

#[derive(Debug)]
pub enum SecurityError {
    InvalidWorkspace,
    PathEscapesWorkspace,
    ReparsePoint(PathBuf),
    InvalidUrl(String),
    DomainDenied(String),
    AddressDenied(IpAddr),
    MissingPinnedAddress,
    InvalidSecretKey,
    UnknownKey(String),
    InvalidCiphertext,
    InsecureProductionKey,
}

impl fmt::Display for SecurityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidWorkspace => formatter.write_str("workspace root is invalid"),
            Self::PathEscapesWorkspace => formatter.write_str("path escapes workspace"),
            Self::ReparsePoint(path) => write!(
                formatter,
                "path contains a reparse point: {}",
                path.display()
            ),
            Self::InvalidUrl(reason) => write!(formatter, "invalid URL: {reason}"),
            Self::DomainDenied(host) => write!(formatter, "domain is not allowed: {host}"),
            Self::AddressDenied(address) => {
                write!(formatter, "network address is not public: {address}")
            }
            Self::MissingPinnedAddress => formatter.write_str("DNS produced no pinned address"),
            Self::InvalidSecretKey => formatter.write_str("secret key is invalid"),
            Self::UnknownKey(key) => write!(formatter, "unknown secret key id: {key}"),
            Self::InvalidCiphertext => formatter.write_str("secret ciphertext is invalid"),
            Self::InsecureProductionKey => {
                formatter.write_str("production secret key is missing or insecure")
            }
        }
    }
}

impl std::error::Error for SecurityError {}

#[derive(Clone, Debug)]
pub struct Workspace {
    root: PathBuf,
}

impl Workspace {
    pub fn new(root: impl AsRef<Path>) -> Result<Self, SecurityError> {
        let root = std::fs::canonicalize(root).map_err(|_| SecurityError::InvalidWorkspace)?;
        if !root.is_dir() {
            return Err(SecurityError::InvalidWorkspace);
        }
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn confine_existing(&self, requested: impl AsRef<Path>) -> Result<PathBuf, SecurityError> {
        let candidate = self.lexical_candidate(requested.as_ref())?;
        reject_links(&self.root, &candidate)?;
        let canonical =
            std::fs::canonicalize(&candidate).map_err(|_| SecurityError::PathEscapesWorkspace)?;
        if !canonical.starts_with(&self.root) {
            return Err(SecurityError::PathEscapesWorkspace);
        }
        Ok(canonical)
    }

    pub fn confine_for_create(
        &self,
        requested: impl AsRef<Path>,
    ) -> Result<PathBuf, SecurityError> {
        let candidate = self.lexical_candidate(requested.as_ref())?;
        let mut existing = candidate.as_path();
        while !existing.exists() {
            existing = existing
                .parent()
                .ok_or(SecurityError::PathEscapesWorkspace)?;
        }
        reject_links(&self.root, existing)?;
        let canonical_parent =
            std::fs::canonicalize(existing).map_err(|_| SecurityError::PathEscapesWorkspace)?;
        if !canonical_parent.starts_with(&self.root) {
            return Err(SecurityError::PathEscapesWorkspace);
        }
        Ok(candidate)
    }

    fn lexical_candidate(&self, requested: &Path) -> Result<PathBuf, SecurityError> {
        let candidate = if requested.is_absolute() {
            requested.to_path_buf()
        } else {
            self.root.join(requested)
        };
        let mut depth = 0usize;
        for component in candidate
            .strip_prefix(&self.root)
            .map_err(|_| SecurityError::PathEscapesWorkspace)?
            .components()
        {
            match component {
                Component::Normal(_) => depth += 1,
                Component::CurDir => {}
                Component::ParentDir if depth > 0 => depth -= 1,
                Component::ParentDir => return Err(SecurityError::PathEscapesWorkspace),
                Component::Prefix(_) | Component::RootDir => {
                    return Err(SecurityError::PathEscapesWorkspace);
                }
            }
        }
        Ok(candidate)
    }
}

fn reject_links(root: &Path, candidate: &Path) -> Result<(), SecurityError> {
    let mut current = root.to_path_buf();
    let relative = candidate
        .strip_prefix(root)
        .map_err(|_| SecurityError::PathEscapesWorkspace)?;
    for component in relative.components() {
        if let Component::Normal(part) = component {
            current.push(part);
            if !current.exists() {
                break;
            }
            let metadata = std::fs::symlink_metadata(&current)
                .map_err(|_| SecurityError::PathEscapesWorkspace)?;
            if is_link_or_reparse(&metadata) {
                return Err(SecurityError::ReparsePoint(current));
            }
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn is_link_or_reparse(metadata: &std::fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[cfg(windows)]
fn is_link_or_reparse(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt as _;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    metadata.file_type().is_symlink()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[derive(Clone, Debug)]
pub struct NetworkPolicy {
    allowed_domains: BTreeSet<String>,
    pub max_redirects: u8,
    pub max_response_bytes: u64,
    pub timeout: std::time::Duration,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PinnedTarget {
    pub url: Url,
    pub host: String,
    pub addresses: Vec<IpAddr>,
}

impl NetworkPolicy {
    pub fn new(allowed_domains: impl IntoIterator<Item = String>) -> Self {
        Self {
            allowed_domains: allowed_domains
                .into_iter()
                .map(|domain| domain.trim_start_matches('.').to_ascii_lowercase())
                .collect(),
            max_redirects: 5,
            max_response_bytes: 10 * 1024 * 1024,
            timeout: std::time::Duration::from_secs(30),
        }
    }

    /// Validates a URL against one DNS answer. The returned addresses are the
    /// only addresses a network client may connect to, preventing re-resolution.
    pub fn pin(
        &self,
        raw_url: &str,
        resolved: impl IntoIterator<Item = IpAddr>,
    ) -> Result<PinnedTarget, SecurityError> {
        let url =
            Url::parse(raw_url).map_err(|error| SecurityError::InvalidUrl(error.to_string()))?;
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err(SecurityError::InvalidUrl(
                "scheme, credentials, or fragment denied".to_owned(),
            ));
        }
        let parsed_host = url
            .host()
            .ok_or_else(|| SecurityError::InvalidUrl("missing host".to_owned()))?;
        let (host, addresses) = match parsed_host {
            Host::Domain(domain) => {
                let host = domain.trim_end_matches('.').to_ascii_lowercase();
                (host, resolved.into_iter().collect::<Vec<_>>())
            }
            Host::Ipv4(address) => (address.to_string(), vec![IpAddr::V4(address)]),
            Host::Ipv6(address) => (address.to_string(), vec![IpAddr::V6(address)]),
        };
        if !self.allowed_domains.is_empty()
            && !self
                .allowed_domains
                .iter()
                .any(|allowed| host == *allowed || host.ends_with(&format!(".{allowed}")))
        {
            return Err(SecurityError::DomainDenied(host));
        }
        if addresses.is_empty() {
            return Err(SecurityError::MissingPinnedAddress);
        }
        if let Some(address) = addresses.iter().find(|address| !is_public_ip(**address)) {
            return Err(SecurityError::AddressDenied(*address));
        }
        Ok(PinnedTarget {
            url,
            host,
            addresses,
        })
    }

    pub fn validate_redirect(
        &self,
        redirect_count: u8,
        raw_url: &str,
        resolved: impl IntoIterator<Item = IpAddr>,
    ) -> Result<PinnedTarget, SecurityError> {
        if redirect_count >= self.max_redirects {
            return Err(SecurityError::InvalidUrl(
                "redirect limit exceeded".to_owned(),
            ));
        }
        self.pin(raw_url, resolved)
    }
}

pub fn is_public_ip(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => is_public_v4(address),
        IpAddr::V6(address) => is_public_v6(address),
    }
}

fn is_public_v4(address: Ipv4Addr) -> bool {
    let [first, second, ..] = address.octets();
    !(address.is_private()
        || address.is_loopback()
        || address.is_link_local()
        || address.is_broadcast()
        || address.is_documentation()
        || address.is_multicast()
        || address.is_unspecified()
        || first == 0
        || (first == 100 && (64..=127).contains(&second))
        || (first == 192 && second == 0)
        || (first == 192 && second == 88)
        || (first == 198 && (18..=19).contains(&second))
        || first >= 240)
}

fn is_public_v6(address: Ipv6Addr) -> bool {
    if let Some(mapped) = address.to_ipv4_mapped() {
        return is_public_v4(mapped);
    }
    let segments = address.segments();
    !(address.is_loopback()
        || address.is_multicast()
        || address.is_unspecified()
        || (segments[0] & 0xfe00) == 0xfc00
        || (segments[0] & 0xffc0) == 0xfe80
        || (segments[0] == 0x2001 && segments[1] == 0x0db8))
}

static SECRET_PATTERNS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    vec![
        (Regex::new(r"(?is)-----BEGIN (?:RSA |EC |OPENSSH |DSA )?PRIVATE KEY-----.*?-----END (?:RSA |EC |OPENSSH |DSA )?PRIVATE KEY-----").expect("valid regex"), "[REDACTED:private-key]"),
        (Regex::new(r"(?i)(bearer\s+)[A-Za-z0-9._~+/=-]{20,}").expect("valid regex"), "$1[REDACTED:bearer]"),
        (Regex::new(r"\bgh[pousr]_[A-Za-z0-9]{36}\b").expect("valid regex"), "[REDACTED:github-token]"),
        (Regex::new(r"\bxox[bpsa]-[A-Za-z0-9-]{10,}\b").expect("valid regex"), "[REDACTED:slack-token]"),
        (Regex::new(r"\bAKIA[A-Z0-9]{16}\b").expect("valid regex"), "[REDACTED:aws-key]"),
        (Regex::new(r"(?i)\b((?:sk|pk|rk|key|api[_-]?key|token)[_-])[A-Za-z0-9_-]{20,}").expect("valid regex"), "$1[REDACTED:api-key]"),
        (Regex::new(r#"(?i)((?:api[_-]?key|secret|password|passwd|token|auth|credential|private[_-]?key)\s*[:=]\s*)([^\s,;'\"\r\n]{8,})"#).expect("valid regex"), "$1[REDACTED]"),
    ]
});

pub fn mask_secrets(text: &str) -> String {
    SECRET_PATTERNS
        .iter()
        .fold(text.to_owned(), |value, (pattern, replacement)| {
            pattern.replace_all(&value, *replacement).into_owned()
        })
}

pub fn mask_json(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(text) => *text = mask_secrets(text),
        serde_json::Value::Array(values) => values.iter_mut().for_each(mask_json),
        serde_json::Value::Object(values) => values.values_mut().for_each(mask_json),
        _ => {}
    }
}

#[derive(Clone)]
pub struct SecretKey {
    id: String,
    cipher: Fernet,
}

impl SecretKey {
    pub fn from_secret(
        id: impl Into<String>,
        secret: &str,
        production: bool,
    ) -> Result<Self, SecurityError> {
        if production
            && (secret.is_empty()
                || secret == "CHANGE_ME"
                || secret == "placeholder-do-not-use-in-production")
        {
            return Err(SecurityError::InsecureProductionKey);
        }
        let source = if !production && (secret.is_empty() || secret == "CHANGE_ME") {
            "placeholder-do-not-use-in-production"
        } else {
            secret
        };
        let normalized = if Fernet::new(source).is_some() {
            source.to_owned()
        } else {
            let digest = Sha256::digest(source.as_bytes());
            base64::engine::general_purpose::URL_SAFE.encode(digest)
        };
        let cipher = Fernet::new(&normalized).ok_or(SecurityError::InvalidSecretKey)?;
        Ok(Self {
            id: id.into(),
            cipher,
        })
    }
}

#[derive(Clone)]
pub struct SecretKeyring {
    active: SecretKey,
    readers: BTreeMap<String, SecretKey>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct VersionedSecret {
    version: u8,
    key_id: String,
    ciphertext: String,
}

impl SecretKeyring {
    pub fn new(active: SecretKey, previous: impl IntoIterator<Item = SecretKey>) -> Self {
        let mut readers = previous
            .into_iter()
            .map(|key| (key.id.clone(), key))
            .collect::<BTreeMap<_, _>>();
        readers.insert(active.id.clone(), active.clone());
        Self { active, readers }
    }

    pub fn encrypt(&self, plaintext: &str) -> Result<String, SecurityError> {
        serde_json::to_string(&VersionedSecret {
            version: 1,
            key_id: self.active.id.clone(),
            ciphertext: self.active.cipher.encrypt(plaintext.as_bytes()),
        })
        .map_err(|_| SecurityError::InvalidCiphertext)
    }

    /// Reads M6's versioned format and raw Python Fernet tokens.
    pub fn decrypt(&self, stored: &str) -> Result<String, SecurityError> {
        if stored.starts_with('{') {
            let envelope: VersionedSecret =
                serde_json::from_str(stored).map_err(|_| SecurityError::InvalidCiphertext)?;
            if envelope.version != 1 {
                return Err(SecurityError::InvalidCiphertext);
            }
            let key = self
                .readers
                .get(&envelope.key_id)
                .ok_or_else(|| SecurityError::UnknownKey(envelope.key_id.clone()))?;
            return decrypt_with(key, &envelope.ciphertext);
        }
        self.readers
            .values()
            .find_map(|key| decrypt_with(key, stored).ok())
            .ok_or(SecurityError::InvalidCiphertext)
    }

    pub fn rotate(&self, stored: &str) -> Result<String, SecurityError> {
        self.encrypt(&self.decrypt(stored)?)
    }
}

fn decrypt_with(key: &SecretKey, ciphertext: &str) -> Result<String, SecurityError> {
    let bytes = key
        .cipher
        .decrypt(ciphertext)
        .map_err(|_| SecurityError::InvalidCiphertext)?;
    String::from_utf8(bytes).map_err(|_| SecurityError::InvalidCiphertext)
}

pub fn sanitize_environment<'a>(
    environment: impl IntoIterator<Item = (&'a str, &'a str)>,
    explicit_allow: &BTreeSet<String>,
) -> BTreeMap<String, String> {
    environment
        .into_iter()
        .filter(|(name, _)| {
            let upper = name.to_ascii_uppercase();
            explicit_allow.contains(&upper)
                || (![
                    "KEY",
                    "SECRET",
                    "PASSWORD",
                    "PASSWD",
                    "TOKEN",
                    "AUTH",
                    "CREDENTIAL",
                    "CREDENTIALS",
                ]
                .iter()
                .any(|marker| upper == *marker || upper.ends_with(&format!("_{marker}")))
                    && !matches!(
                        upper.as_str(),
                        "AWS_ACCESS_KEY_ID" | "GOOGLE_APPLICATION_CREDENTIALS"
                    ))
        })
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect()
}
