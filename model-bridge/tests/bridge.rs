use doclify_model_bridge::{Bridge, BridgeError, Capability, Request};
use serde_json::json;
use std::{os::unix::fs::PermissionsExt, time::Duration};

fn fake(script: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("claude");
    std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    (dir, path)
}
fn request() -> Request {
    Request {
        prompt: "sensitive input".into(),
        schema: json!({"type":"object","required":["answer"],"properties":{"answer":{"type":"string"}}}),
        model: None,
        timeout: Duration::from_secs(2),
    }
}
const HELP: &str = "--print --output-format --json-schema --tools --restricted --safe-mode --strict-mcp-config --no-session-persistence --disable-slash-commands --permission-mode --permission-prompts";
#[tokio::test]
async fn returns_only_schema_validated_result() {
    let (_d, path) = fake(&format!("if [ \"$1\" = --help ]; then echo '{HELP}'; else cat >/dev/null; echo '{{\"is_error\":false,\"structured_output\":{{\"answer\":\"ok\"}}}}'; fi"));
    let bridge = Bridge::claude(path);
    assert_eq!(
        bridge.capability().await,
        Capability::ConstrainedAuthenticationUnverified
    );
    assert_eq!(
        bridge.execute(request()).await.unwrap(),
        json!({"answer":"ok"})
    );
}
#[tokio::test]
async fn refuses_missing_capabilities_and_codex() {
    let (_d, path) = fake("echo --print");
    assert!(matches!(
        Bridge::claude(path.clone()).execute(request()).await,
        Err(BridgeError::Unsupported(_))
    ));
    assert!(matches!(
        Bridge::codex(path).execute(request()).await,
        Err(BridgeError::Unsupported(_))
    ));
}
#[tokio::test]
async fn codex_fails_closed_without_spawning() {
    let (dir, path) = fake(&format!("touch \"$(dirname \"$0\")/ran\"; echo '{HELP}'"));
    let bridge = Bridge::codex(path);
    assert!(matches!(
        bridge.capability().await,
        Capability::Unavailable(_)
    ));
    assert!(matches!(
        bridge.execute(request()).await,
        Err(BridgeError::Unsupported(_))
    ));
    assert!(!dir.path().join("ran").exists());
}
#[tokio::test]
async fn preserves_process_failure_and_rejects_invalid_output() {
    let (_d, path) = fake(&format!(
        "if [ \"$1\" = --help ]; then echo '{HELP}'; else echo quota-exceeded >&2; exit 7; fi"
    ));
    match Bridge::claude(path).execute(request()).await {
        Err(BridgeError::Process { code, stderr, .. }) => {
            assert_eq!(code, Some(7));
            assert!(stderr.contains("quota-exceeded"));
        }
        other => panic!("{other:?}"),
    }
    let (_d, path) = fake(&format!("if [ \"$1\" = --help ]; then echo '{HELP}'; else echo '{{\"structured_output\":{{\"answer\":42}}}}'; fi"));
    assert!(matches!(
        Bridge::claude(path).execute(request()).await,
        Err(BridgeError::InvalidOutput(_))
    ));
}
#[tokio::test]
async fn rejects_provider_error_envelopes_and_times_out() {
    let (_d, path) = fake(&format!("if [ \"$1\" = --help ]; then echo '{HELP}'; else echo '{{\"is_error\":true,\"result\":\"authentication failed\"}}'; fi"));
    assert!(matches!(
        Bridge::claude(path).execute(request()).await,
        Err(BridgeError::Provider(_))
    ));
    let (_d, path) = fake(&format!(
        "if [ \"$1\" = --help ]; then echo '{HELP}'; else exec sleep 5; fi"
    ));
    let mut req = request();
    req.timeout = Duration::from_millis(50);
    assert!(matches!(
        Bridge::claude(path).execute(req).await,
        Err(BridgeError::Timeout)
    ));
}
