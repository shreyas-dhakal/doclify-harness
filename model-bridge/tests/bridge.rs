use doclify_model_bridge::{codex_visible_tools, Bridge, BridgeError, Capability, Request};
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
const INIT: &str =
    r#"{"type":"system","subtype":"init","tools":["StructuredOutput"],"mcp_servers":[]}"#;
const HELP: &str = "--print --output-format --json-schema --verbose --tools --restricted --safe-mode --strict-mcp-config --no-session-persistence --disable-slash-commands --permission-mode --permission-prompts";
#[tokio::test]
async fn returns_only_schema_validated_result() {
    let (_d, path) = fake(&format!("if [ \"$1\" = --help ]; then echo '{HELP}'; else cat >/dev/null; echo '{INIT}'; echo '{{\"type\":\"result\",\"is_error\":false,\"structured_output\":{{\"answer\":\"ok\"}}}}'; fi"));
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
    let (_d, path) = fake(&format!("if [ \"$1\" = --help ]; then echo '{HELP}'; else echo '{INIT}'; echo '{{\"type\":\"result\",\"structured_output\":{{\"answer\":42}}}}'; fi"));
    assert!(matches!(
        Bridge::claude(path).execute(request()).await,
        Err(BridgeError::InvalidOutput(_))
    ));
}
#[tokio::test]
async fn rejects_provider_error_envelopes_and_times_out() {
    let (_d, path) = fake(&format!("if [ \"$1\" = --help ]; then echo '{HELP}'; else echo '{INIT}'; echo '{{\"type\":\"result\",\"is_error\":true,\"result\":\"authentication failed\"}}'; fi"));
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
#[tokio::test]
async fn kills_runs_whose_init_exposes_more_than_structured_output() {
    let init = INIT.replace("[\"StructuredOutput\"]", "[\"StructuredOutput\",\"Bash\"]");
    let (dir, path) = fake(&format!("if [ \"$1\" = --help ]; then echo '{HELP}'; else echo '{init}'; sleep 1; touch \"$(dirname \"$0\")/continued\"; fi"));
    match Bridge::claude(path).execute(request()).await {
        Err(BridgeError::Unsupported(reason)) => assert!(reason.contains("Bash"), "{reason}"),
        other => panic!("{other:?}"),
    }
    std::thread::sleep(Duration::from_millis(1500));
    assert!(!dir.path().join("continued").exists());
    let mcp = INIT.replace("\"mcp_servers\":[]", "\"mcp_servers\":[{\"name\":\"x\"}]");
    let (_d, path) = fake(&format!(
        "if [ \"$1\" = --help ]; then echo '{HELP}'; else echo '{mcp}'; fi"
    ));
    assert!(matches!(
        Bridge::claude(path).execute(request()).await,
        Err(BridgeError::Unsupported(_))
    ));
}
#[tokio::test]
async fn requires_init_before_result() {
    for script in [
        "echo '{\"type\":\"result\",\"structured_output\":{\"answer\":\"ok\"}}'",
        "cat >/dev/null",
    ] {
        let (_d, path) = fake(&format!(
            "if [ \"$1\" = --help ]; then echo '{HELP}'; else {script}; fi"
        ));
        assert!(matches!(
            Bridge::claude(path).execute(request()).await,
            Err(BridgeError::Unsupported(_))
        ));
    }
}
#[tokio::test]
async fn codex_diagnostic_lists_rendered_tools_from_empty_home() {
    let (_d, path) = fake(
        r#"[ "$1 $2" = "debug prompt-input" ] && [ "$3" = --disable ] && [ "$(pwd -P)" = "$(cd "$CODEX_HOME" && pwd -P)" ] && [ -z "$(ls -A "$CODEX_HOME")" ] || exit 3
echo '[{"type":"additional_tools","tools":[{"type":"namespace","name":"functions","tools":[{"name":"exec"}]},{"name":"spawn_agent"}]},{"type":"message"}]'"#,
    );
    assert_eq!(
        codex_visible_tools(&path, &["--disable", "shell_tool"])
            .await
            .unwrap(),
        ["functions.exec", "spawn_agent"]
    );
    let (_d, path) = fake(r#"echo '[{"type":"message"}]'"#);
    assert!(matches!(
        codex_visible_tools(&path, &[]).await,
        Err(BridgeError::InvalidOutput(_))
    ));
}
/// Opt-in: `DOCLIFY_LIVE_CLAUDE=1` runs one authenticated inference with the installed `claude`,
/// using its own login in place; `execute` itself verifies the runtime tool list.
#[tokio::test]
async fn live_claude_runtime_constraint() {
    if std::env::var_os("DOCLIFY_LIVE_CLAUDE").is_none() {
        return;
    }
    let mut req = request();
    req.prompt = "Reply with answer \"ok\".".into();
    req.model = Some("haiku".into());
    req.timeout = Duration::from_secs(120);
    let value = Bridge::claude("claude").execute(req).await.unwrap();
    assert!(value["answer"].is_string());
}
/// Opt-in: `DOCLIFY_LIVE_CODEX=1` renders the installed Codex's tools without credentials; fails
/// once a release offers a tool-free mode, signalling the Codex bridge can be revisited.
#[tokio::test]
async fn live_codex_still_exposes_tools() {
    if std::env::var_os("DOCLIFY_LIVE_CODEX").is_none() {
        return;
    }
    let tools = codex_visible_tools("codex", &[]).await.unwrap();
    assert!(tools.iter().any(|t| t.ends_with("exec")), "{tools:?}");
}
