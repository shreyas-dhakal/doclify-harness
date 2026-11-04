use doclify_capture::queue::Queue;
use serde_json::json;
use std::fs;

#[test]
fn reconciliation_replays_partial_and_changed_transcripts_without_duplicates() {
    let root = tempfile::tempdir().unwrap();
    let transcript = root.path().join("session.jsonl");
    let queue = Queue::open(root.path().join("local")).unwrap();
    let event = json!({"agent":"claude","payload":{"session_id":"s","transcript_path":transcript}});
    fs::write(&transcript, "{\"type\":\"user\",\"uuid\":\"1\",\"message\":{\"content\":\"hello\"}}\n{\"type\":\"assistant\"").unwrap();
    queue.reconcile_with_roots(&event, &[root.path()]).unwrap();
    queue.reconcile_with_roots(&event, &[root.path()]).unwrap();
    assert_eq!(queue.pending().unwrap().len(), 1);
    fs::write(&transcript, "{\"type\":\"user\",\"uuid\":\"1\",\"message\":{\"content\":\"hello\"}}\n{\"type\":\"assistant\",\"uuid\":\"2\",\"message\":{\"content\":\"reply\"}}\n").unwrap();
    queue.reconcile_with_roots(&event, &[root.path()]).unwrap();
    assert_eq!(queue.pending().unwrap().len(), 2);
}
/// Unsupported lines no longer strand the valid entries after them: those are
/// queued, the rest is recorded with an actionable reason, and replay re-reads
/// the session from its stored event once the adapter or file is fixed.
#[test]
fn unsupported_lines_are_reported_and_recoverable_by_replay() {
    let root = tempfile::tempdir().unwrap();
    let transcript = root.path().join("session.jsonl");
    let queue = Queue::open(root.path().join("local")).unwrap();
    let event = json!({"agent":"claude","payload":{"session_id":"s","transcript_path":transcript}});
    let valid =
        "{\"type\":\"user\",\"uuid\":\"1\",\"message\":{\"content\":\"allow guest checkout\"}}\n";
    fs::write(&transcript, format!("{{\"type\":\"brand-new\"}}\n{valid}")).unwrap();
    queue.reconcile_with_roots(&event, &[root.path()]).unwrap();
    assert_eq!(
        queue.pending().unwrap().len(),
        1,
        "valid entry after unsupported line"
    );
    let reports = queue.unsupported().unwrap();
    assert_eq!(reports.len(), 1);
    let reason = reports[0]["reason"].as_str().unwrap();
    assert!(
        reason.contains("claude brand-new") && reason.contains("offset 0"),
        "{reason}"
    );
    assert!(reason.contains("replay"), "{reason}");
    assert_eq!(reports[0]["event"], event);

    // Unchanged input: still reported, nothing duplicated.
    queue.reconcile_with_roots(&event, &[root.path()]).unwrap();
    assert_eq!(queue.replay_unsupported(&[root.path()]).unwrap(), 0);
    assert_eq!(queue.pending().unwrap().len(), 1);
    assert_eq!(queue.unsupported().unwrap().len(), 1);

    // Once the line is understood, replay recovers it and clears the report.
    fs::write(
        &transcript,
        format!(
            "{{\"type\":\"assistant\",\"uuid\":\"0\",\"message\":{{\"content\":\"ok\"}}}}\n{valid}"
        ),
    )
    .unwrap();
    assert_eq!(queue.replay_unsupported(&[root.path()]).unwrap(), 1);
    assert_eq!(queue.pending().unwrap().len(), 2);
    assert!(queue.unsupported().unwrap().is_empty());
}

/// A report written by an older adapter is re-read from the start, so an
/// upgrade recovers lines the checkpoint had already skipped.
#[test]
fn adapter_upgrade_rereads_previously_unsupported_sessions() {
    let root = tempfile::tempdir().unwrap();
    let transcript = root.path().join("session.jsonl");
    let queue = Queue::open(root.path().join("local")).unwrap();
    let event = json!({"agent":"claude","payload":{"session_id":"s","transcript_path":transcript}});
    fs::write(&transcript, "{\"type\":\"brand-new\"}\n").unwrap();
    queue.reconcile_with_roots(&event, &[root.path()]).unwrap();
    let report = fs::read_dir(root.path().join("local/unsupported"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let mut stored: serde_json::Value =
        serde_json::from_slice(&fs::read(&report).unwrap()).unwrap();
    stored["format"] = json!("claude-session/0.0");
    fs::write(&report, serde_json::to_vec(&stored).unwrap()).unwrap();
    queue.reconcile_with_roots(&event, &[root.path()]).unwrap();
    let again: serde_json::Value = serde_json::from_slice(&fs::read(&report).unwrap()).unwrap();
    assert_eq!(
        again["format"],
        doclify_capture::transcript::CLAUDE_FORMAT,
        "re-read from 0"
    );
}

#[test]
fn malformed_hook_queue_file_is_reported_and_retained() {
    let root = tempfile::tempdir().unwrap();
    let hooks = root.path().join("hooks");
    fs::create_dir(&hooks).unwrap();
    let bad = hooks.join("event.json");
    fs::write(&bad, "broken").unwrap();
    let queue = Queue::open(root.path().join("local")).unwrap();
    assert!(queue.drain_hooks(&hooks).is_err());
    assert!(bad.exists());
}

#[test]
fn rejects_untrusted_transcript_before_reading() {
    let dir = tempfile::tempdir().unwrap();
    let secret = dir.path().join("secret");
    fs::write(&secret, "not a transcript\n").unwrap();
    let queue = Queue::open(dir.path().join("queue")).unwrap();
    let event = json!({"agent":"claude","payload":{"session_id":"s","transcript_path":secret}});
    assert_eq!(
        queue.reconcile(&event).unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied
    );
}

#[cfg(unix)]
#[test]
fn raw_capture_files_are_private() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let queue = Queue::open(dir.path().join("queue")).unwrap();
    queue
        .reconcile(&json!({"agent":"claude","payload":{}}))
        .unwrap();
    let path = fs::read_dir(dir.path().join("queue/events"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert_eq!(fs::metadata(path).unwrap().permissions().mode() & 0o077, 0);
}

#[cfg(unix)]
#[test]
fn transcript_symlink_cannot_escape_authorized_root() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    let sessions = dir.path().join("sessions");
    fs::create_dir(&sessions).unwrap();
    fs::write(dir.path().join("secret"), "secret\n").unwrap();
    symlink(dir.path().join("secret"), sessions.join("escape.jsonl")).unwrap();
    let queue = Queue::open(dir.path().join("queue")).unwrap();
    let event = json!({"agent":"claude","payload":{"session_id":"s","transcript_path":sessions.join("escape.jsonl")}});
    assert!(queue.reconcile_with_roots(&event, &[&sessions]).is_err());
}

#[test]
fn worker_lock_serializes_independent_queue_handles() {
    use std::{sync::mpsc, time::Duration};
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("queue");
    let first = Queue::open(&root).unwrap();
    let held = first.lock().unwrap();
    let (tx, rx) = mpsc::channel();
    let thread = std::thread::spawn(move || {
        let other = Queue::open(root).unwrap();
        let _held = other.lock().unwrap();
        tx.send(()).unwrap();
    });
    assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
    drop(held);
    rx.recv_timeout(Duration::from_secs(3)).unwrap();
    thread.join().unwrap();
}

#[test]
fn changed_discovery_snapshot_is_not_processed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    fs::write(
        &path,
        "{\"type\":\"user\",\"message\":{\"content\":\"new\"}}\n",
    )
    .unwrap();
    let queue = Queue::open(dir.path().join("queue")).unwrap();
    let event = json!({"agent":"claude","payload":{"session_id":"s","transcript_path":path}});
    assert!(queue
        .reconcile_snapshot(&event, &[dir.path()], Some(b"old\n"))
        .is_err());
    assert!(queue.pending().unwrap().is_empty());
}

#[test]
fn tool_results_are_retained_without_creating_proposal_jobs() {
    let dir = tempfile::tempdir().unwrap();
    let queue = Queue::open(dir.path()).unwrap();
    let event = serde_json::json!({"agent":"claude","payload":{"hook_event_name":"PostToolUse","tool_input":{"command":"cargo test"}}});
    queue.reconcile(&event).unwrap();
    assert!(queue.pending().unwrap().is_empty());
    assert_eq!(fs::read_dir(dir.path().join("events")).unwrap().count(), 1);
}

#[test]
fn transcript_jobs_carry_hook_metadata_not_its_prompt() {
    let root = tempfile::tempdir().unwrap();
    let transcript = root.path().join("session.jsonl");
    let queue = Queue::open(root.path().join("local")).unwrap();
    fs::write(&transcript, "{\"type\":\"user\",\"uuid\":\"1\",\"message\":{\"content\":\"allow guest checkout\"}}\n{\"type\":\"assistant\",\"uuid\":\"2\",\"message\":{\"content\":\"ok\"}}\n").unwrap();
    let event = json!({"agent":"claude","head":"main","payload":{"session_id":"s","parent_session_id":"parent","scope":{"branch":"feature"},"paths":["src"],"hook_event_name":"Stop","transcript_path":transcript,"prompt":"allow guest checkout","last_assistant_message":"ok"}});
    queue.reconcile_with_roots(&event, &[root.path()]).unwrap();
    let jobs = queue.pending().unwrap();
    assert_eq!(jobs.len(), 2);
    for (_, job) in jobs {
        let p = &job["context"]["payload"];
        assert_eq!(p["session_id"], "s");
        assert_eq!(p["parent_session_id"], "parent");
        assert_eq!(p["scope"]["branch"], "feature");
        assert_eq!(p["paths"][0], "src");
        assert_eq!(job["context"]["head"], "main");
        assert!(
            p.get("prompt").is_none() && p.get("last_assistant_message").is_none(),
            "{job}"
        );
    }
}

/// Codex subagent rollouts and Claude sidechain entries carry their parent
/// session, and the parent survives incremental reads past the metadata line.
#[test]
fn child_sessions_carry_parent_session_metadata() {
    let root = tempfile::tempdir().unwrap();
    let queue = Queue::open(root.path().join("local")).unwrap();
    let rollout = root.path().join("child.jsonl");
    fs::write(&rollout, concat!(
        r#"{"ordinal":0,"type":"session_meta","payload":{"id":"child","cwd":"/r","source":{"subagent":{"thread_spawn":{"parent_thread_id":"parent","depth":1}}}}}"#, "\n",
    )).unwrap();
    let event = json!({"agent":"codex","payload":{"session_id":"child","transcript_path":rollout}});
    queue.reconcile_with_roots(&event, &[root.path()]).unwrap();
    let mut f = fs::OpenOptions::new().append(true).open(&rollout).unwrap();
    std::io::Write::write_all(&mut f, concat!(r#"{"ordinal":1,"type":"response_item","payload":{"type":"message","id":"m1","role":"user","content":[{"type":"input_text","text":"later"}]}}"#, "\n").as_bytes()).unwrap();
    queue.reconcile_with_roots(&event, &[root.path()]).unwrap();
    let jobs = queue.pending().unwrap();
    assert_eq!(jobs.len(), 2);
    for (_, job) in &jobs {
        assert_eq!(
            job["context"]["payload"]["parent_session_id"], "parent",
            "{job}"
        );
    }

    let side = root.path().join("agent-a1.jsonl");
    fs::write(&side, concat!(
        r#"{"type":"user","uuid":"c1","isSidechain":true,"agentId":"a1","sessionId":"parent","cwd":"/r","message":{"content":"subtask"}}"#, "\n",
    )).unwrap();
    let event = json!({"agent":"claude","payload":{"session_id":"parent","transcript_path":side}});
    queue.reconcile_with_roots(&event, &[root.path()]).unwrap();
    let job = queue
        .pending()
        .unwrap()
        .into_iter()
        .map(|(_, j)| j)
        .find(|j| j["agent"] == "claude")
        .unwrap();
    assert_eq!(job["session_id"], "a1");
    assert_eq!(job["context"]["payload"]["parent_session_id"], "parent");
}

/// Doclify's own startup context, and an agent quoting it back, never become
/// extraction input; new content in the same message still does.
#[test]
fn injected_context_is_not_recaptured() {
    let root = tempfile::tempdir().unwrap();
    let queue = Queue::open(root.path().join("local")).unwrap();
    let transcript = root.path().join("session.jsonl");
    let line = |uuid: &str, kind: &str, text: &str| {
        format!(
            "{}\n",
            json!({"type":kind,"uuid":uuid,"message":{"content":text}})
        )
    };
    fs::write(&transcript, [
        line("1", "user", "# Doclify project context\nGenerated locally from Doclify records (no model call).\n\n## Accepted constraints\n- [decision-guest@v2] (decision, implementation planned) Allow guest checkout\n  Rationale: Customers abandon account creation.\n"),
        line("2", "assistant", "Constraints:\n- [decision-guest@v2] Allow guest checkout\n  Rationale: Customers abandon account creation.\nNext I will add receipt emails."),
    ].concat()).unwrap();
    let event = json!({"agent":"claude","payload":{"session_id":"s","transcript_path":transcript}});
    queue.reconcile_with_roots(&event, &[root.path()]).unwrap();
    let texts: Vec<String> = queue
        .pending()
        .unwrap()
        .into_iter()
        .map(|(_, j)| j["text"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(texts, ["Constraints:\nNext I will add receipt emails."]);
}

/// A session recorded at this path before the repository existed (a deleted
/// repository whose cwd was reused) is not imported; a current one is.
#[test]
fn sessions_predating_the_repository_are_not_imported() {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("repo");
    fs::create_dir(&repo).unwrap();
    assert!(std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&repo)
        .status()
        .unwrap()
        .success());
    let queue = Queue::open(root.path().join("local")).unwrap();
    let transcript = root.path().join("old.jsonl");
    fs::write(
        &transcript,
        "{\"type\":\"user\",\"uuid\":\"1\",\"message\":{\"content\":\"old project\"}}\n",
    )
    .unwrap();
    let day_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(86_400);
    fs::File::options()
        .write(true)
        .open(&transcript)
        .unwrap()
        .set_modified(day_ago)
        .unwrap();
    let event = json!({"agent":"claude","worktree":repo,"payload":{"session_id":"s","transcript_path":transcript}});
    queue.reconcile_with_roots(&event, &[root.path()]).unwrap();
    assert!(queue.pending().unwrap().is_empty());
    fs::File::options()
        .write(true)
        .open(&transcript)
        .unwrap()
        .set_modified(std::time::SystemTime::now())
        .unwrap();
    queue.reconcile_with_roots(&event, &[root.path()]).unwrap();
    assert_eq!(queue.pending().unwrap().len(), 1);
}
