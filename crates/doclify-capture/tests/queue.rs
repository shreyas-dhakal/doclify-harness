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
    queue.reconcile(&event).unwrap();
    queue.reconcile(&event).unwrap();
    assert_eq!(queue.pending().unwrap().len(), 2);
    fs::write(&transcript, "{\"type\":\"user\",\"uuid\":\"1\",\"message\":{\"content\":\"hello\"}}\n{\"type\":\"assistant\",\"uuid\":\"2\",\"message\":{\"content\":\"reply\"}}\n").unwrap();
    queue.reconcile(&event).unwrap();
    assert_eq!(queue.pending().unwrap().len(), 3);
}
#[test]
fn unsupported_transcript_stays_pending_until_corrected() {
    let root = tempfile::tempdir().unwrap();
    let transcript = root.path().join("session.jsonl");
    let queue = Queue::open(root.path().join("local")).unwrap();
    let event = json!({"agent":"codex","payload":{"session_id":"s","transcript_path":transcript}});
    fs::write(&transcript, "{\"type\":\"new-format\"}\n").unwrap();
    assert!(queue.reconcile(&event).is_err());
    fs::write(
        &transcript,
        "{\"type\":\"session_meta\",\"payload\":{\"id\":\"s\"}}\n",
    )
    .unwrap();
    queue.reconcile(&event).unwrap();
    assert_eq!(queue.pending().unwrap().len(), 2);
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
