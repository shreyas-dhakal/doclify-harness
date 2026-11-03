use doclify_domain::{ImplementationState, Provenance, Record, RecordKind, ReviewState, Store};
use std::{fs, process::Command};
fn run(dir: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_doclify"))
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap()
}
#[test]
fn review_and_handoff_only_surface_accepted_decisions() {
    let dir = tempfile::tempdir().unwrap();
    assert!(Command::new("git")
        .args(["init", "-q"])
        .current_dir(dir.path())
        .status()
        .unwrap()
        .success());
    let store = Store::open(dir.path()).unwrap();
    let r = Record {
        id: "guest".into(),
        version: 1,
        kind: RecordKind::Decision,
        title: "Guest checkout".into(),
        statement: "Allow guests".into(),
        rationale: "Reduce abandonment".into(),
        review: ReviewState::Proposed,
        implementation: ImplementationState::Partial,
        experiment_outcome: None,
        provenance: vec![Provenance {
            source: "session-1".into(),
            excerpt: None,
        }],
        evidence: vec!["evidence-1".into()],
        supersedes: None,
    };
    store.propose(&r).unwrap();
    let output = run(dir.path(), &["handoff"]);
    assert!(output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("Allow guests"));
    assert!(run(dir.path(), &["review", "guest", "1", "accept"])
        .status
        .success());
    let output = run(dir.path(), &["handoff"]);
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains("Allow guests")
            && text.contains("Reduce abandonment")
            && text.contains("evidence-1")
    );
}
#[test]
fn init_installs_hooks_and_status_reports_queue() {
    let dir = tempfile::tempdir().unwrap();
    Command::new("git")
        .args(["init", "-q"])
        .current_dir(dir.path())
        .status()
        .unwrap();
    assert!(run(dir.path(), &["init"]).status.success());
    assert!(
        fs::read_to_string(dir.path().join(".claude/settings.local.json"))
            .unwrap()
            .contains("doclify")
    );
    let output = run(dir.path(), &["status"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("pending_events"));
}

#[test]
fn hook_is_silent_and_fail_open_without_a_repository() {
    let dir = tempfile::tempdir().unwrap();
    let output = run(dir.path(), &["hook", "codex"]);
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
}

#[test]
fn model_configuration_and_sync_report_pending_work() {
    let dir = tempfile::tempdir().unwrap();
    Command::new("git")
        .args(["init", "-q"])
        .current_dir(dir.path())
        .status()
        .unwrap();
    assert!(run(dir.path(), &["init"]).status.success());
    let out = run(
        dir.path(),
        &[
            "model",
            "set",
            "openai",
            "http://127.0.0.1:9999",
            "TEST_DOCLIFY_KEY",
            "test-model",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = run(dir.path(), &["model"]);
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["selected"]["model"], "test-model");
    assert!(run(dir.path(), &["sync"]).status.success());
    let out = run(dir.path(), &["status"]);
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["pending_extraction"], 0);
    assert!(run(dir.path(), &["doctor"]).status.success());
}

#[test]
fn repl_accepts_model_picker_and_exit() {
    use std::io::Write;
    use std::process::Stdio;
    let dir = tempfile::tempdir().unwrap();
    Command::new("git")
        .args(["init", "-q"])
        .current_dir(dir.path())
        .status()
        .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_doclify"))
        .current_dir(dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"/model\nclaude\n/quit\n")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    let model = run(dir.path(), &["model"]);
    let value: serde_json::Value = serde_json::from_slice(&model.stdout).unwrap();
    assert_eq!(value["selected"]["provider"], "claude");
}

#[test]
fn model_configuration_rejects_credentials_in_url() {
    let dir = tempfile::tempdir().unwrap();
    Command::new("git")
        .args(["init", "-q"])
        .current_dir(dir.path())
        .status()
        .unwrap();
    let out = run(
        dir.path(),
        &[
            "model",
            "set",
            "openai",
            "https://secret@example.com/v1",
            "API_KEY",
            "test",
        ],
    );
    assert!(!out.status.success());
    assert!(!dir.path().join(".git/doclify/model.json").exists());
}

#[test]
fn status_before_init_reports_unregistered() {
    let dir = tempfile::tempdir().unwrap();
    Command::new("git")
        .args(["init", "-q"])
        .current_dir(dir.path())
        .status()
        .unwrap();
    let out = run(dir.path(), &["status"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let status: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(status["registered"], false);
}
