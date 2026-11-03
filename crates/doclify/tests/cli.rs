use doclify_domain::{ImplementationState, Provenance, Record, RecordKind, ReviewState, Store};
use std::{fs, process::Command};
fn run(dir: &std::path::Path, args: &[&str]) -> std::process::Output {
    use std::os::unix::fs::PermissionsExt;
    let home = doclify_capture::RepoInfo::discover(dir)
        .map(|repo| repo.git_dir.join("test-home"))
        .unwrap_or_else(|| dir.join("test-home"));
    let bin = home.join("bin");
    fs::create_dir_all(&bin).unwrap();
    for name in ["launchctl", "systemctl"] {
        let path = bin.join(name);
        fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    Command::new(env!("CARGO_BIN_EXE_doclify"))
        .current_dir(dir)
        .env("HOME", home)
        .env(
            "PATH",
            format!(
                "{}:{}",
                bin.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
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
        scope: None,
        session: None,
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

#[test]
fn sync_rejects_untrusted_transcript_path_without_removing_hook() {
    use std::io::Write;
    use std::process::Stdio;
    let dir = tempfile::tempdir().unwrap();
    Command::new("git")
        .args(["init", "-q"])
        .current_dir(dir.path())
        .status()
        .unwrap();
    assert!(run(dir.path(), &["init"]).status.success());
    let secret = dir.path().join("secret.jsonl");
    fs::write(&secret, "secret\n").unwrap();
    let mut hook = Command::new(env!("CARGO_BIN_EXE_doclify"))
        .args(["hook", "claude"])
        .current_dir(dir.path())
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    write!(
        hook.stdin.take().unwrap(),
        "{}",
        serde_json::json!({"cwd":dir.path(),"session_id":"s","transcript_path":secret})
    )
    .unwrap();
    assert!(hook.wait().unwrap().success());
    let out = run(dir.path(), &["sync"]);
    assert!(out.status.success());
    assert_eq!(
        fs::read_dir(dir.path().join(".git/doclify/queue/rejected"))
            .unwrap()
            .count(),
        1
    );
}

#[test]
fn sync_extracts_and_reviews_a_captured_event() {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::process::Stdio;
    let dir = tempfile::tempdir().unwrap();
    Command::new("git")
        .args(["init", "-q"])
        .current_dir(dir.path())
        .status()
        .unwrap();
    assert!(run(dir.path(), &["init"]).status.success());
    let mut hook = Command::new(env!("CARGO_BIN_EXE_doclify"))
        .args(["hook", "claude"])
        .current_dir(dir.path())
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    write!(
        hook.stdin.take().unwrap(),
        "{}",
        serde_json::json!({"cwd":dir.path(),"message":"allow guest checkout"})
    )
    .unwrap();
    assert!(hook.wait().unwrap().success());
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    assert!(run(
        dir.path(),
        &[
            "model",
            "set",
            "openai",
            &url,
            "DOCLIFY_TEST_KEY",
            "fixture"
        ]
    )
    .status
    .success());
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut data = Vec::new();
        let mut buf = [0; 4096];
        let body_offset = loop {
            let n = stream.read(&mut buf).unwrap();
            assert!(n > 0);
            data.extend_from_slice(&buf[..n]);
            if let Some(pos) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                break pos + 4;
            }
        };
        let headers = String::from_utf8_lossy(&data[..body_offset]);
        let length: usize = headers
            .lines()
            .find_map(|line| {
                line.split_once(':')
                    .filter(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                    .map(|(_, v)| v.trim().parse().unwrap())
            })
            .unwrap();
        while data.len() < body_offset + length {
            let n = stream.read(&mut buf).unwrap();
            assert!(n > 0);
            data.extend_from_slice(&buf[..n]);
        }
        let request: serde_json::Value =
            serde_json::from_slice(&data[body_offset..body_offset + length]).unwrap();
        let input: serde_json::Value =
            serde_json::from_str(request["messages"][1]["content"].as_str().unwrap()).unwrap();
        let result = serde_json::json!({"proposals":[{"kind":"decision","summary":"Allow guest checkout","review":"proposed","evidence":[input["source_id"]]}]}).to_string();
        let response = serde_json::json!({"choices":[{"message":{"content":result}}]}).to_string();
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", response.len(), response).unwrap();
    });
    let out = Command::new(env!("CARGO_BIN_EXE_doclify"))
        .args(["sync"])
        .current_dir(dir.path())
        .env("DOCLIFY_TEST_KEY", "fixture")
        .output()
        .unwrap();
    server.join().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let review: serde_json::Value =
        serde_json::from_slice(&run(dir.path(), &["review"]).stdout).unwrap();
    assert_eq!(
        review["pending_review"][0]["statement"],
        "Allow guest checkout"
    );
    let id = review["pending_review"][0]["id"].as_str().unwrap();
    assert!(run(dir.path(), &["review", id, "1", "accept"])
        .status
        .success());
    assert!(
        String::from_utf8_lossy(&run(dir.path(), &["handoff"]).stdout)
            .contains("Allow guest checkout")
    );
}

#[test]
fn repl_filters_model_picker_and_selects_match() {
    use std::io::Write;
    use std::process::Stdio;
    let dir = tempfile::tempdir().unwrap();
    assert!(Command::new("git")
        .args(["init", "-q"])
        .current_dir(dir.path())
        .status()
        .unwrap()
        .success());
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
        .write_all(b"/model\ncode\n1\n/quit\n")
        .unwrap();
    assert!(child.wait_with_output().unwrap().status.success());
    let model: serde_json::Value =
        serde_json::from_slice(&run(dir.path(), &["model"]).stdout).unwrap();
    assert_eq!(model["selected"]["provider"], "codex");
}

#[test]
fn sync_processes_valid_transcript_hook() {
    let dir = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    Command::new("git")
        .args(["init", "-q"])
        .current_dir(dir.path())
        .status()
        .unwrap();
    assert!(run(dir.path(), &["init"]).status.success());
    let sessions = home.path().join(".codex/sessions");
    fs::create_dir_all(&sessions).unwrap();
    let transcript = sessions.join("session.jsonl");
    fs::write(&transcript, format!("{}\n{}\n", serde_json::json!({"type":"session_meta","payload":{"id":"s","cwd":dir.path()}}), serde_json::json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"hello"}]}}))).unwrap();
    doclify_capture::hook::enqueue(
        doclify_capture::Agent::Codex,
        serde_json::json!({"cwd":dir.path(),"session_id":"s","transcript_path":transcript})
            .to_string()
            .as_bytes(),
        dir.path(),
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_doclify"))
        .arg("sync")
        .current_dir(dir.path())
        .env("HOME", home.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        fs::read_dir(dir.path().join(".git/doclify/queue"))
            .unwrap()
            .count(),
        0
    );
    assert!(
        fs::read_dir(dir.path().join(".git/doclify/jobs/pending"))
            .unwrap()
            .count()
            >= 2
    );
}

#[test]
fn sync_waits_for_the_background_worker_queue_lock() {
    use std::time::Duration;
    let dir = tempfile::tempdir().unwrap();
    Command::new("git")
        .args(["init", "-q"])
        .current_dir(dir.path())
        .status()
        .unwrap();
    assert!(run(dir.path(), &["init"]).status.success());
    let queue = doclify_capture::queue::Queue::open(dir.path().join(".git/doclify/jobs")).unwrap();
    let held = queue.lock().unwrap();
    let mut sync = Command::new(env!("CARGO_BIN_EXE_doclify"))
        .arg("sync")
        .current_dir(dir.path())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert!(sync.try_wait().unwrap().is_none());
    drop(held);
    assert!(sync.wait().unwrap().success());
}

#[test]
fn linked_worktrees_keep_capture_queues_separate() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    let other = dir.path().join("other");
    fs::create_dir(&repo).unwrap();
    let git = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .current_dir(&repo)
            .status()
            .unwrap()
    };
    assert!(git(&["init", "-q"]).success());
    assert!(git(&[
        "-c",
        "user.name=Test",
        "-c",
        "user.email=test@example.com",
        "commit",
        "--allow-empty",
        "-qm",
        "base"
    ])
    .success());
    assert!(git(&["worktree", "add", "-qb", "other", other.to_str().unwrap()]).success());
    assert!(run(&repo, &["init"]).status.success());
    assert!(run(&other, &["init"]).status.success());
    doclify_capture::hook::enqueue(
        doclify_capture::Agent::Codex,
        br#"{"message":"other worktree"}"#,
        &other,
    )
    .unwrap();
    assert!(run(&repo, &["sync"]).status.success());
    let other_info = doclify_capture::RepoInfo::discover(&other).unwrap();
    assert_eq!(
        doclify_capture::hook::pending(&other_info.queue_dir())
            .unwrap()
            .len(),
        1
    );
    assert!(run(&other, &["sync"]).status.success());
    assert!(doclify_capture::hook::pending(&other_info.queue_dir())
        .unwrap()
        .is_empty());
    let main_info = doclify_capture::RepoInfo::discover(&repo).unwrap();
    assert_ne!(main_info.git_dir, other_info.git_dir);
    assert!(
        doclify_capture::queue::Queue::open(main_info.git_dir.join("doclify/jobs"))
            .unwrap()
            .pending()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        doclify_capture::queue::Queue::open(other_info.git_dir.join("doclify/jobs"))
            .unwrap()
            .pending()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn repl_request_persists_proposal_for_explicit_review() {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::process::Stdio;
    let dir = tempfile::tempdir().unwrap();
    assert!(Command::new("git")
        .args(["init", "-q"])
        .current_dir(dir.path())
        .status()
        .unwrap()
        .success());
    let store = Store::open(dir.path()).unwrap();
    store
        .propose(&Record {
            id: "existing".into(),
            version: 1,
            kind: RecordKind::Decision,
            title: "Existing".into(),
            statement: "Existing policy".into(),
            rationale: "reason".into(),
            review: ReviewState::Proposed,
            implementation: ImplementationState::Unknown,
            experiment_outcome: None,
            provenance: vec![Provenance {
                source: "source".into(),
                excerpt: None,
            }],
            evidence: vec!["evidence-123".into()],
            supersedes: None,
            scope: None,
            session: None,
        })
        .unwrap();
    store.review("existing", 1, ReviewState::Accepted).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    assert!(run(
        dir.path(),
        &[
            "model",
            "set",
            "openai",
            &url,
            "DOCLIFY_REPL_KEY",
            "fixture"
        ]
    )
    .status
    .success());
    listener.set_nonblocking(true).unwrap();
    let server = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let (mut stream, _) = loop {
            match listener.accept() {
                Ok(pair) => break pair,
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        && std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(10))
                }
                Err(e) => panic!("no request: {e}"),
            }
        };
        let mut data = Vec::new();
        let mut buf = [0; 4096];
        let offset = loop {
            let n = stream.read(&mut buf).unwrap();
            assert!(n > 0);
            data.extend_from_slice(&buf[..n]);
            if let Some(p) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                break p + 4;
            }
        };
        let headers = String::from_utf8_lossy(&data[..offset]);
        let length: usize = headers
            .lines()
            .find_map(|l| {
                l.split_once(':')
                    .filter(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                    .map(|(_, v)| v.trim().parse().unwrap())
            })
            .unwrap();
        while data.len() < offset + length {
            let n = stream.read(&mut buf).unwrap();
            assert!(n > 0);
            data.extend_from_slice(&buf[..n]);
        }
        assert!(String::from_utf8_lossy(&data[offset..]).contains("evidence-123"));
        let result = serde_json::json!({"answer":"Suggested change", "proposals":[{"kind":"decision","summary":"New policy","review":"proposed","evidence":["evidence-123"]}]}).to_string();
        let response = serde_json::json!({"choices":[{"message":{"content":result}}]}).to_string();
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", response.len(), response).unwrap();
    });
    let mut child = Command::new(env!("CARGO_BIN_EXE_doclify"))
        .current_dir(dir.path())
        .env("DOCLIFY_REPL_KEY", "fixture")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"Update the policy\n/quit\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    server.join().unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("Suggested change"));
    let pending: serde_json::Value =
        serde_json::from_slice(&run(dir.path(), &["review"]).stdout).unwrap();
    assert_eq!(pending["pending_review"][0]["statement"], "New policy");
    assert_eq!(pending["pending_review"][0]["review"], "proposed");
}

#[test]
fn repl_discovers_filters_and_selects_api_model_without_losing_configuration() {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::process::Stdio;
    let dir = tempfile::tempdir().unwrap();
    assert!(Command::new("git")
        .args(["init", "-q"])
        .current_dir(dir.path())
        .status()
        .unwrap()
        .success());
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    assert!(run(
        dir.path(),
        &["model", "set", "openai", &url, "DOCLIFY_REPL_KEY", "old"]
    )
    .status
    .success());
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0; 4096];
        let n = stream.read(&mut buf).unwrap();
        assert!(String::from_utf8_lossy(&buf[..n]).starts_with("GET /models "));
        let body = r#"{"data":[{"id":"Alpha-Model"},{"id":"Beta-Model"}]}"#;
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
    });
    let mut child = Command::new(env!("CARGO_BIN_EXE_doclify"))
        .current_dir(dir.path())
        .env("DOCLIFY_REPL_KEY", "fixture")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"/model\nBETA\n1\n/quit\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    server.join().unwrap();
    assert!(output.status.success());
    let selected: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.path().join(".git/doclify/model.json")).unwrap())
            .unwrap();
    assert_eq!(selected["model"], "Beta-Model");
    assert_eq!(selected["provider"], "openai");
    assert_eq!(selected["base_url"], url);
    assert_eq!(selected["api_key_env"], "DOCLIFY_REPL_KEY");
}

#[test]
fn rejected_events_explain_failure_and_can_be_replayed() {
    let dir = tempfile::tempdir().unwrap();
    Command::new("git")
        .args(["init", "-q"])
        .current_dir(dir.path())
        .status()
        .unwrap();
    assert!(run(dir.path(), &["init"]).status.success());
    let queue = dir.path().join(".git/doclify/queue");
    fs::write(
        queue.join("bad.json"),
        br#"{"agent":"claude","payload":{"transcript_path":"/outside","session_id":"s"}}"#,
    )
    .unwrap();
    assert!(run(dir.path(), &["sync"]).status.success());
    let result = run(dir.path(), &["rejected"]);
    let list: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert!(list["events"][0]["reason"]
        .as_str()
        .unwrap()
        .contains("transcript"));
    let id = list["events"][0]["id"].as_str().unwrap();
    let result = run(dir.path(), &["rejected", "replay", id]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(queue.join(id).exists());
}

#[test]
fn picker_keeps_configured_model_when_discovery_fails() {
    use std::io::Write;
    use std::process::Stdio;
    let dir = tempfile::tempdir().unwrap();
    Command::new("git")
        .args(["init", "-q"])
        .current_dir(dir.path())
        .status()
        .unwrap();
    assert!(run(
        dir.path(),
        &[
            "model",
            "set",
            "openai",
            "http://127.0.0.1:1",
            "DOCLIFY_MISSING_TEST_KEY",
            "offline-model"
        ]
    )
    .status
    .success());
    let mut child = Command::new(env!("CARGO_BIN_EXE_doclify"))
        .current_dir(dir.path())
        .env_remove("DOCLIFY_MISSING_TEST_KEY")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"/model\n\n/quit\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains("offline-model (API availability and authentication unverified)"),
        "{text}"
    );
    assert!(text.contains("\"capability\": \"unavailable\""));
}
