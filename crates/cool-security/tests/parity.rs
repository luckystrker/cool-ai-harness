use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr};

use cool_security::{
    Capability, CapabilityPolicy, Decision, NetworkPolicy, SecretKey, SecretKeyring, SecurityError,
    Workspace, mask_json, mask_secrets, sanitize_environment,
};
use tempfile::tempdir;

#[test]
fn capability_and_tool_decisions_merge_to_the_stricter_result() {
    let mut core = CapabilityPolicy::new(None);
    core.set(Capability::Write, Decision::Ask);
    core.set(Capability::Network, Decision::Deny);
    assert_eq!(
        core.evaluate([Capability::Read, Capability::Write], Decision::Allow)
            .effective,
        Decision::Ask
    );
    let mut plugin = CapabilityPolicy::new(Some(Decision::Allow));
    plugin.set(Capability::Network, Decision::Allow);
    assert_eq!(
        core.narrow_with(&plugin).resolve(Capability::Network),
        Decision::Deny
    );
}

#[test]
fn workspace_rejects_absolute_and_parent_escape_before_io() {
    let directory = tempdir().unwrap();
    let workspace = Workspace::new(directory.path()).unwrap();
    assert!(workspace.confine_for_create("nested/file.txt").is_ok());
    assert!(matches!(
        workspace.confine_for_create("../escape.txt"),
        Err(SecurityError::PathEscapesWorkspace)
    ));
    let outside = directory.path().parent().unwrap().join("outside.txt");
    assert!(matches!(
        workspace.confine_for_create(outside),
        Err(SecurityError::PathEscapesWorkspace)
    ));
}

#[test]
fn network_policy_pins_public_dns_and_rechecks_redirects() {
    let policy = NetworkPolicy::new(["example.com".to_owned()]);
    let public = IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34));
    let target = policy
        .pin("https://api.example.com/data", [public])
        .unwrap();
    assert_eq!(target.addresses, vec![public]);
    assert!(matches!(
        policy.pin(
            "https://api.example.com/data",
            [IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))]
        ),
        Err(SecurityError::AddressDenied(_))
    ));
    assert!(matches!(
        policy.pin("http://8.8.8.8/", []),
        Err(SecurityError::DomainDenied(_))
    ));
    let any_public_domain = NetworkPolicy::new([]);
    assert!(matches!(
        any_public_domain.pin("http://127.0.0.1/", [public]),
        Err(SecurityError::AddressDenied(_))
    ));
    assert!(matches!(
        any_public_domain.pin("http://[::ffff:127.0.0.1]/", []),
        Err(SecurityError::AddressDenied(_))
    ));
    assert!(matches!(
        policy.pin("https://example.com.evil.test/", [public]),
        Err(SecurityError::DomainDenied(_))
    ));
    assert!(matches!(
        policy.pin("https://user:pass@example.com/", [public]),
        Err(SecurityError::InvalidUrl(_))
    ));
}

#[test]
fn network_policy_can_allow_only_explicit_loopback_targets() {
    let policy = NetworkPolicy::new(["localhost".to_owned()]).loopback_only();
    let pinned = policy
        .pin("http://localhost:11434/v1", ["127.0.0.1".parse().unwrap()])
        .unwrap();
    assert_eq!(pinned.host, "localhost");
    assert!(matches!(
        policy.pin("http://localhost/v1", ["192.168.1.10".parse().unwrap()]),
        Err(SecurityError::AddressDenied(_))
    ));
    assert!(matches!(
        policy.pin("http://localhost/v1", ["93.184.216.34".parse().unwrap()]),
        Err(SecurityError::AddressDenied(_))
    ));
}

#[test]
fn secret_filter_masks_text_nested_json_and_environment() {
    let value = "Authorization: Bearer abcdefghijklmnopqrstuvwxyz123456";
    assert_eq!(
        mask_secrets(value),
        "Authorization: Bearer [REDACTED:bearer]"
    );
    assert_eq!(mask_secrets(&mask_secrets(value)), mask_secrets(value));
    let mut json = serde_json::json!({
        "nested": ["token=abcdefgh12345678"],
        "api_key": "short-plain-value",
        "safe": {"password": "another-short-value"}
    });
    mask_json(&mut json);
    assert_eq!(json["nested"][0], "token=[REDACTED]");
    assert_eq!(json["api_key"], "[REDACTED]");
    assert_eq!(json["safe"]["password"], "[REDACTED]");
    let allow = BTreeSet::from(["TOOL_TOKEN".to_owned()]);
    let env = sanitize_environment(
        [
            ("PATH", "bin"),
            ("OPENAI_API_KEY", "secret"),
            ("AWS_ACCESS_KEY_ID", "secret"),
            ("GOOGLE_APPLICATION_CREDENTIALS", "secret.json"),
            ("TOOL_TOKEN", "allowed"),
        ],
        &allow,
    );
    assert_eq!(env.get("PATH").map(String::as_str), Some("bin"));
    assert!(!env.contains_key("OPENAI_API_KEY"));
    assert!(!env.contains_key("AWS_ACCESS_KEY_ID"));
    assert!(!env.contains_key("GOOGLE_APPLICATION_CREDENTIALS"));
    assert_eq!(env.get("TOOL_TOKEN").map(String::as_str), Some("allowed"));
}

#[test]
fn versioned_secret_reader_accepts_python_fernet_and_rotates_keys() {
    let old = SecretKey::from_secret("old", "legacy passphrase", false).unwrap();
    let old_ring = SecretKeyring::new(old.clone(), []);
    let versioned = old_ring.encrypt("hello").unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&versioned).unwrap();
    let python_compatible_token = parsed["ciphertext"].as_str().unwrap();
    assert_eq!(old_ring.decrypt(python_compatible_token).unwrap(), "hello");

    let current = SecretKey::from_secret("current", "new passphrase", false).unwrap();
    let rotated_ring = SecretKeyring::new(current, [old]);
    let rotated = rotated_ring.rotate(python_compatible_token).unwrap();
    assert_eq!(rotated_ring.decrypt(&rotated).unwrap(), "hello");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&rotated).unwrap()["keyId"],
        "current"
    );
}

#[test]
fn decrypts_a_token_emitted_by_python_cryptography() {
    let key = SecretKey::from_secret(
        "python",
        "kEHo8HDAG3WG6ZU-KDuJPgmiBuB6idegIF0Z-HHbzO8=",
        false,
    )
    .unwrap();
    let ring = SecretKeyring::new(key, []);
    assert_eq!(
        ring.decrypt("gAAAAABqlvzH096dXjJThfKeigB2k5aSexmVr3QKQRXT2TTQab41v7mnQCAi2mxuuGWl0P1duuUTIrjP5K7Xu8Z3uq56-f470DuxBTG17eZeDGLAVZHp5Xc=")
            .unwrap(),
        "python-to-rust-m6"
    );
}

#[test]
fn production_rejects_placeholder_secret_keys() {
    assert!(matches!(
        SecretKey::from_secret("active", "CHANGE_ME", true),
        Err(SecurityError::InsecureProductionKey)
    ));
}
