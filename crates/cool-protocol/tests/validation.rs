//! Boundary validation for protocol input types.

use cool_protocol::{CommandEnvelope, IdempotencyKey, V1Version};

#[test]
fn idempotency_keys_are_bounded() {
    assert!(IdempotencyKey::new("").is_err());
    assert!(IdempotencyKey::new("key-1").is_ok());
    assert!(IdempotencyKey::new("k".repeat(256)).is_ok());
    assert!(IdempotencyKey::new("k".repeat(257)).is_err());
}

#[test]
fn command_envelopes_reject_other_protocol_versions() {
    let parsed = serde_json::from_value::<CommandEnvelope>(serde_json::json!({
        "protocolVersion": 1,
        "commandId": "command-1",
        "command": {"method": "memory.stats", "params": {}},
    }))
    .expect("version 1 parses");
    assert_eq!(parsed.protocol_version, V1Version::VALUE);

    let error = serde_json::from_value::<CommandEnvelope>(serde_json::json!({
        "protocolVersion": 2,
        "commandId": "command-1",
        "command": {"method": "memory.stats", "params": {}},
    }))
    .expect_err("version 2 must be rejected");
    assert!(error.to_string().contains("only App Protocol version 1"));
}
