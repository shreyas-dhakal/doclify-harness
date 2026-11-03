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
#[test]
fn unsupported_transcript_stays_pending_until_corrected() {
    let root = tempfile::tempdir().unwrap();
    let transcript = root.path().join("session.jsonl");
    let queue = Queue::open(root.path().join("local")).unwrap();
    let event = json!({"agent":"codex","payload":{"session_id":"s","transcript_path":transcript}});
    fs::write(&transcript, "{\"type\":\"new-format\"}\n").unwrap();
    assert!(queue.reconcile_with_roots(&event, &[root.path()]).is_err());
    fs::write(
        &transcript,
        "{\"type\":\"session_meta\",\"payload\":{\"id\":\"s\"}}\n",
    )
    .unwrap();
    queue.reconcile_with_roots(&event, &[root.path()]).unwrap();
    assert_eq!(queue.pending().unwrap().len(), 1);
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
