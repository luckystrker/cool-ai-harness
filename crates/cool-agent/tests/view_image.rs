use std::sync::Arc;

use cool_agent::{ArtifactReader, ModelContentPart, ToolContext, builtin_registry};
use cool_security::{Capability, CapabilityPolicy, Decision, Workspace};
use serde_json::json;
use tempfile::tempdir;

fn context(root: &std::path::Path) -> ToolContext {
    ToolContext::new(
        Workspace::new(root).unwrap(),
        CapabilityPolicy::new(Some(Decision::Allow)),
    )
}

const PNG_BYTES: &[u8] = b"\x89PNG\r\n\x1a\n";

#[tokio::test]
async fn view_image_is_a_read_allow_tool() {
    let tool = builtin_registry().get("view_image").unwrap();
    assert!(tool.capabilities.contains(&Capability::Read));
    assert_eq!(tool.default_decision, Decision::Allow);
}

#[tokio::test]
async fn view_image_returns_pixels_as_an_output_part() {
    let directory = tempdir().unwrap();
    std::fs::write(directory.path().join("shot.png"), PNG_BYTES).unwrap();
    let tool = builtin_registry().get("view_image").unwrap();
    let result = tool
        .execute(&context(directory.path()), json!({"path": "shot.png"}))
        .await
        .unwrap();
    assert!(!result.is_error);
    assert_eq!(result.output["mediaType"], "image/png");
    assert_eq!(result.output["bytes"], PNG_BYTES.len() as u64);
    let parts = result.output_parts.as_deref().expect("image output parts");
    let [
        ModelContentPart::Image {
            media_type,
            data_base64,
        },
    ] = parts
    else {
        panic!("expected a single image part, got {parts:?}")
    };
    assert_eq!(media_type, "image/png");
    use base64::Engine as _;
    assert_eq!(
        data_base64,
        &base64::engine::general_purpose::STANDARD.encode(PNG_BYTES)
    );
}

#[tokio::test]
async fn view_image_requires_exactly_one_of_path_or_artifact() {
    let directory = tempdir().unwrap();
    let tool = builtin_registry().get("view_image").unwrap();
    for arguments in [json!({}), json!({"path": "a.png", "artifactId": "1"})] {
        let error = tool
            .execute(&context(directory.path()), arguments)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("exactly one"), "{error}");
    }
}

#[tokio::test]
async fn view_image_rejects_a_non_image_extension() {
    let directory = tempdir().unwrap();
    std::fs::write(directory.path().join("notes.txt"), "hello").unwrap();
    let tool = builtin_registry().get("view_image").unwrap();
    let error = tool
        .execute(&context(directory.path()), json!({"path": "notes.txt"}))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("unsupported image"), "{error}");
}

#[derive(Debug)]
struct StubReader {
    media_type: &'static str,
}

impl ArtifactReader for StubReader {
    fn read_artifact(&self, artifact_id: &str) -> Result<(String, Vec<u8>), String> {
        assert_eq!(artifact_id, "42");
        Ok((self.media_type.to_owned(), PNG_BYTES.to_vec()))
    }
}

#[tokio::test]
async fn view_image_reads_an_artifact_through_the_runtime_reader() {
    let directory = tempdir().unwrap();
    let context = context(directory.path()).with_artifact_reader(Arc::new(StubReader {
        media_type: "image/png",
    }));
    let tool = builtin_registry().get("view_image").unwrap();
    let result = tool
        .execute(&context, json!({"artifactId": "42"}))
        .await
        .unwrap();
    assert!(!result.is_error);
    assert_eq!(result.output["artifactId"], "42");
    assert_eq!(result.output["mediaType"], "image/png");
    assert!(matches!(
        result.output_parts.as_deref(),
        Some([ModelContentPart::Image { .. }])
    ));
}

#[tokio::test]
async fn view_image_rejects_a_non_image_artifact() {
    let directory = tempdir().unwrap();
    let context = context(directory.path()).with_artifact_reader(Arc::new(StubReader {
        media_type: "text/plain",
    }));
    let tool = builtin_registry().get("view_image").unwrap();
    let error = tool
        .execute(&context, json!({"artifactId": "42"}))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("is not an image"), "{error}");
}

#[tokio::test]
async fn view_image_artifact_mode_fails_without_a_reader() {
    let directory = tempdir().unwrap();
    let tool = builtin_registry().get("view_image").unwrap();
    let error = tool
        .execute(&context(directory.path()), json!({"artifactId": "42"}))
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("unavailable in this runtime"),
        "{error}"
    );
}
