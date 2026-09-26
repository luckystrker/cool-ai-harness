//! Publisher signatures and the plugin transparency log.
//!
//! A publisher may ship `signature.json` at the plugin root carrying an
//! Ed25519 signature over the canonical payload
//! `"cool-plugin-signature-v1\n{name}\n{version}\n{contentHash}"`. The file is
//! excluded from the content hash itself so the signed value is stable.
//!
//! Signature verification resolves the publisher's key from the store-level
//! keyring `trusted-publishers.json` (`{"publishers": {"name": "<base64 raw
//! 32-byte public key>"}}`). An unsigned plugin is loadable and flagged;
//! a signature that fails to parse, verify, or match the trusted keyring is a
//! load blocker — tamper evidence must never pass as unsigned content.
//!
//! Every managed lifecycle change (install, update, enable, disable, remove)
//! is also appended to the store's `transparency-log.jsonl`: an append-only,
//! hash-chained JSONL record where `entryHash` covers the whole entry
//! including `prevHash`, so history edits or removals are detectable. The
//! latest `entryHash` is mirrored into `transparency-head.txt` after each
//! append, so truncating the tail of the log is detectable too; `append` is
//! always preceded by a full chain verification, so a corrupted log refuses
//! to extend instead of silently continuing.

use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Name of the publisher signature file at the plugin root.
pub const SIGNATURE_FILE: &str = "signature.json";
/// Name of the publisher keyring at the plugin store root.
pub const KEYRING_FILE: &str = "trusted-publishers.json";
/// Name of the append-only transparency log at the plugin store root.
pub const TRANSPARENCY_FILE: &str = "transparency-log.jsonl";
/// Sidecar carrying the last entry's hash, so tail truncation is detectable.
pub const TRANSPARENCY_HEAD_FILE: &str = "transparency-head.txt";

/// Canonical bytes a publisher signs for a plugin release.
pub fn signature_payload(name: &str, version: &str, content_hash: &str) -> Vec<u8> {
    format!("cool-plugin-signature-v1\n{name}\n{version}\n{content_hash}").into_bytes()
}

/// A verified publisher signature attached to a loaded bundle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedSignature {
    pub publisher: String,
    /// Hex-encoded SHA-256 of the publisher's raw public key.
    pub key_fingerprint: String,
}

/// Outcome of signature inspection for a plugin tree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SignatureStatus {
    /// No `signature.json` shipped; nothing cryptographically asserts origin.
    Unsigned,
    /// Signature verifies against a trusted publisher key.
    Signed(VerifiedSignature),
    /// A signature file exists but cannot be trusted — blocked, never silent.
    Invalid(String),
}

impl SignatureStatus {
    pub fn publisher(&self) -> Option<&str> {
        match self {
            Self::Signed(signature) => Some(signature.publisher.as_str()),
            _ => None,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Unsigned => "unsigned",
            Self::Signed(_) => "signed",
            Self::Invalid(_) => "invalid",
        }
    }
}

#[derive(Debug)]
pub enum SignatureError {
    Io(std::io::Error),
    Json(serde_json::Error),
    Malformed(String),
}

impl fmt::Display for SignatureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "signature I/O error: {error}"),
            Self::Json(error) => write!(formatter, "signature JSON error: {error}"),
            Self::Malformed(message) => write!(formatter, "malformed signature: {message}"),
        }
    }
}

impl std::error::Error for SignatureError {}

impl From<std::io::Error> for SignatureError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_json::Error> for SignatureError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SignatureFile {
    publisher: String,
    signature: String,
    /// Optional pinned key; when present it must equal the keyring entry, so a
    /// tampered tree cannot substitute a key the store never trusted.
    #[serde(default)]
    public_key: Option<String>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Keyring {
    #[serde(default)]
    publishers: BTreeMap<String, String>,
}

fn key_fingerprint(public_key: &[u8; 32]) -> String {
    format!("{:x}", Sha256::digest(public_key))
}

fn decode_public_key(encoded: &str) -> Result<[u8; 32], SignatureError> {
    let raw = BASE64
        .decode(encoded.trim())
        .map_err(|_| SignatureError::Malformed("publisher key is not base64".to_owned()))?;
    <[u8; 32]>::try_from(raw.as_slice())
        .map_err(|_| SignatureError::Malformed("publisher key is not 32 bytes".to_owned()))
}

/// Loads the trusted publisher keyring from the store root. An absent file
/// means no publisher is trusted — every signed plugin then fails as
/// `Invalid("publisher is not trusted")`.
pub fn load_keyring(store_root: &Path) -> Result<BTreeMap<String, [u8; 32]>, SignatureError> {
    let path = store_root.join(KEYRING_FILE);
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    let keyring: Keyring = serde_json::from_slice(&fs::read(&path)?)?;
    keyring
        .publishers
        .into_iter()
        .map(|(name, key)| decode_public_key(&key).map(|key| (name, key)))
        .collect()
}

/// Inspects `signature.json` inside `root` and verifies it against the
/// keyring found at `store_root` (derived by the caller from the data root).
pub fn inspect_signature(
    root: &Path,
    store_root: &Path,
    name: &str,
    version: &str,
    content_hash: &str,
) -> Result<SignatureStatus, SignatureError> {
    let signature_path = root.join(SIGNATURE_FILE);
    if !signature_path.is_file() {
        return Ok(SignatureStatus::Unsigned);
    }
    let invalid = |reason: String| Ok(SignatureStatus::Invalid(reason));
    let file: SignatureFile = match serde_json::from_slice(&fs::read(&signature_path)?) {
        Ok(file) => file,
        Err(error) => return invalid(format!("signature.json is not parseable: {error}")),
    };
    if file.publisher.trim().is_empty() {
        return invalid("signature.json names an empty publisher".to_owned());
    }
    let keyring = load_keyring(store_root)?;
    let Some(trusted_key) = keyring.get(&file.publisher) else {
        return invalid(format!("publisher \"{}\" is not trusted", file.publisher));
    };
    if let Some(pinned) = &file.public_key {
        let pinned = decode_public_key(pinned)?;
        if &pinned != trusted_key {
            return invalid(
                "signature.json public key does not match the trusted keyring".to_owned(),
            );
        }
    }
    let raw_signature = BASE64
        .decode(file.signature.trim())
        .map_err(|_| SignatureError::Malformed("signature is not base64".to_owned()))?;
    let signature_bytes: [u8; 64] = raw_signature
        .as_slice()
        .try_into()
        .map_err(|_| SignatureError::Malformed("signature is not 64 bytes".to_owned()))?;
    let signature = Signature::from_bytes(&signature_bytes);
    let verifying_key = VerifyingKey::from_bytes(trusted_key)
        .map_err(|_| SignatureError::Malformed("publisher key is not a valid key".to_owned()))?;
    let payload = signature_payload(name, version, content_hash);
    if verifying_key.verify(&payload, &signature).is_err() {
        return invalid("signature does not match the plugin contents".to_owned());
    }
    Ok(SignatureStatus::Signed(VerifiedSignature {
        publisher: file.publisher,
        key_fingerprint: key_fingerprint(trusted_key),
    }))
}

/// Signs a release payload — used by tests, fixtures, and the future signing
/// CLI so publishers produce `signature.json` without shelling out.
pub fn sign_payload(
    signing_key: &ed25519_dalek::SigningKey,
    name: &str,
    version: &str,
    content_hash: &str,
) -> String {
    use ed25519_dalek::Signer as _;
    let signature = signing_key.sign(&signature_payload(name, version, content_hash));
    BASE64.encode(signature.to_bytes())
}

pub fn public_key_base64(signing_key: &ed25519_dalek::SigningKey) -> String {
    BASE64.encode(signing_key.verifying_key().to_bytes())
}

/// One hash-chained transparency log record.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TransparencyEntry {
    pub seq: u64,
    pub at: String,
    pub action: String,
    pub name: String,
    pub version: String,
    pub content_hash: String,
    pub publisher: Option<String>,
    pub signature_status: String,
    pub prev_hash: String,
    pub entry_hash: String,
}

impl TransparencyEntry {
    /// Hash committed into `entry_hash`; covers every field except the hash
    /// itself, serialized deterministically.
    fn sealed_hash(&self) -> Result<String, serde_json::Error> {
        #[derive(Serialize)]
        struct Sealed<'a> {
            seq: u64,
            at: &'a str,
            action: &'a str,
            name: &'a str,
            version: &'a str,
            #[serde(rename = "contentHash")]
            content_hash: &'a str,
            publisher: &'a Option<String>,
            #[serde(rename = "signatureStatus")]
            signature_status: &'a str,
            #[serde(rename = "prevHash")]
            prev_hash: &'a str,
        }
        let sealed = Sealed {
            seq: self.seq,
            at: &self.at,
            action: &self.action,
            name: &self.name,
            version: &self.version,
            content_hash: &self.content_hash,
            publisher: &self.publisher,
            signature_status: &self.signature_status,
            prev_hash: &self.prev_hash,
        };
        let bytes = serde_json::to_vec(&sealed)?;
        Ok(format!("{:x}", Sha256::digest(&bytes)))
    }
}

/// Reads the whole transparency log without validating it.
pub fn read_transparency_log(store_root: &Path) -> Result<Vec<TransparencyEntry>, SignatureError> {
    let path = store_root.join(TRANSPARENCY_FILE);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let mut entries = Vec::new();
    for (index, line) in fs::read_to_string(&path)?.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let entry: TransparencyEntry = serde_json::from_str(line).map_err(|error| {
            SignatureError::Malformed(format!(
                "transparency log line {} is not parseable: {error}",
                index + 1
            ))
        })?;
        entries.push(entry);
    }
    Ok(entries)
}

/// Verifies the transparency log chain: sequence numbers, per-entry hashes,
/// the prev-hash linkage, and the head sidecar (which pins the final hash so
/// tail truncation does not pass silently). Returns the number of verified
/// entries.
pub fn verify_transparency_log(store_root: &Path) -> Result<usize, SignatureError> {
    let entries = read_transparency_log(store_root)?;
    let head_path = store_root.join(TRANSPARENCY_HEAD_FILE);
    let head = match fs::read_to_string(&head_path) {
        Ok(head) => Some(head.trim().to_owned()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(SignatureError::Io(error)),
    };
    if entries.is_empty() != head.is_none() {
        return Err(SignatureError::Malformed(
            "transparency log and head anchor disagree about emptiness".to_owned(),
        ));
    }
    let mut prev_hash = String::new();
    for (index, entry) in entries.iter().enumerate() {
        if entry.seq != index as u64 + 1 {
            return Err(SignatureError::Malformed(format!(
                "transparency log sequence gap at line {}",
                index + 1
            )));
        }
        if entry.prev_hash != prev_hash {
            return Err(SignatureError::Malformed(format!(
                "transparency log chain broken at entry {}",
                entry.seq
            )));
        }
        if entry.sealed_hash().map_err(SignatureError::Json)? != entry.entry_hash {
            return Err(SignatureError::Malformed(format!(
                "transparency log entry {} hash mismatch",
                entry.seq
            )));
        }
        prev_hash = entry.entry_hash.clone();
    }
    if let (Some(head), Some(last)) = (head, entries.last())
        && head != last.entry_hash
    {
        return Err(SignatureError::Malformed(
            "transparency head anchor does not match the last entry — tail was removed".to_owned(),
        ));
    }
    Ok(entries.len())
}

/// Appends one lifecycle record to the store's transparency log. Callers hold
/// the store write lock so the seq/prevHash chain cannot interleave.
pub fn append_transparency(
    store_root: &Path,
    action: &str,
    name: &str,
    version: &str,
    content_hash: &str,
    signature_status: &SignatureStatus,
    timestamp: String,
) -> Result<TransparencyEntry, SignatureError> {
    // Refuse to extend a corrupted or truncated chain: verify first.
    verify_transparency_log(store_root)?;
    let entries = read_transparency_log(store_root)?;
    let prev_hash = entries
        .last()
        .map(|entry| entry.entry_hash.clone())
        .unwrap_or_default();
    let mut entry = TransparencyEntry {
        seq: entries.len() as u64 + 1,
        at: timestamp,
        action: action.to_owned(),
        name: name.to_owned(),
        version: version.to_owned(),
        content_hash: content_hash.to_owned(),
        publisher: signature_status.publisher().map(str::to_owned),
        signature_status: signature_status.label().to_owned(),
        prev_hash,
        entry_hash: String::new(),
    };
    entry.entry_hash = entry.sealed_hash().map_err(SignatureError::Json)?;
    let path: PathBuf = store_root.join(TRANSPARENCY_FILE);
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    use std::io::Write as _;
    writeln!(file, "{}", serde_json::to_string(&entry)?)?;
    file.sync_data()?;
    // Anchor the newest hash in a sidecar so a tail-truncated log verifies
    // against a head the log itself no longer carries.
    fs::write(
        store_root.join(TRANSPARENCY_HEAD_FILE),
        format!("{}\n", entry.entry_hash),
    )?;
    Ok(entry)
}

#[cfg(test)]
mod tests {
    use super::*;

    use ed25519_dalek::SigningKey;

    fn signing_key() -> SigningKey {
        // Fixed seed — a fixture key for round-tripping the payload, not a
        // credential; any 32 bytes is a valid Ed25519 seed.
        SigningKey::from_bytes(&[7u8; 32])
    }

    #[test]
    fn signed_payload_verifies_against_trusted_keyring() {
        let root = tempfile::tempdir().unwrap();
        let store = tempfile::tempdir().unwrap();
        let key = signing_key();
        fs::write(
            store.path().join(KEYRING_FILE),
            serde_json::to_string(&serde_json::json!({
                "publishers": {"acme": public_key_base64(&key)}
            }))
            .unwrap(),
        )
        .unwrap();
        fs::write(
            root.path().join(SIGNATURE_FILE),
            serde_json::to_string(&serde_json::json!({
                "publisher": "acme",
                "signature": sign_payload(&key, "demo", "1.0.0", "abcd"),
            }))
            .unwrap(),
        )
        .unwrap();
        let status = inspect_signature(root.path(), store.path(), "demo", "1.0.0", "abcd").unwrap();
        match status {
            SignatureStatus::Signed(verified) => {
                assert_eq!(verified.publisher, "acme");
                assert_eq!(verified.key_fingerprint.len(), 64);
            }
            other => panic!("expected signed status, got {other:?}"),
        }
        // Same tree, different contents — must not verify.
        let status =
            inspect_signature(root.path(), store.path(), "demo", "1.0.0", "other").unwrap();
        assert!(matches!(status, SignatureStatus::Invalid(_)));
    }

    #[test]
    fn untrusted_publisher_and_missing_signature_are_distinct() {
        let root = tempfile::tempdir().unwrap();
        let store = tempfile::tempdir().unwrap();
        assert_eq!(
            inspect_signature(root.path(), store.path(), "demo", "", "hash").unwrap(),
            SignatureStatus::Unsigned
        );
        let key = signing_key();
        fs::write(
            root.path().join(SIGNATURE_FILE),
            serde_json::to_string(&serde_json::json!({
                "publisher": "stranger",
                "signature": sign_payload(&key, "demo", "", "hash"),
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(matches!(
            inspect_signature(root.path(), store.path(), "demo", "", "hash").unwrap(),
            SignatureStatus::Invalid(_)
        ));
    }

    #[test]
    fn transparency_log_chains_and_detects_edits() {
        let store = tempfile::tempdir().unwrap();
        let signed = SignatureStatus::Signed(VerifiedSignature {
            publisher: "acme".to_owned(),
            key_fingerprint: "f".repeat(64),
        });
        append_transparency(
            store.path(),
            "install",
            "demo",
            "1.0.0",
            "hash1",
            &signed,
            "2026-01-01T00:00:00Z".to_owned(),
        )
        .unwrap();
        append_transparency(
            store.path(),
            "enable",
            "demo",
            "1.0.0",
            "hash1",
            &signed,
            "2026-01-01T00:00:01Z".to_owned(),
        )
        .unwrap();
        assert_eq!(verify_transparency_log(store.path()).unwrap(), 2);

        // Tamper with the first entry's action — the chain must fail.
        let path = store.path().join(TRANSPARENCY_FILE);
        let content = fs::read_to_string(&path).unwrap();
        let mut lines: Vec<String> = content.lines().map(str::to_owned).collect();
        let mut first: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
        first["action"] = serde_json::Value::String("update".to_owned());
        lines[0] = serde_json::to_string(&first).unwrap();
        fs::write(&path, lines.join("\n") + "\n").unwrap();
        assert!(verify_transparency_log(store.path()).is_err());
    }
}
