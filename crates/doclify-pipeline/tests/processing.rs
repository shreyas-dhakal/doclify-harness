use doclify_capture::queue::Queue;
use doclify_pipeline::{process, Extractor};
use serde_json::Value;
use std::io;

struct Provider {
    fail: bool,
}
impl Extractor for Provider {
    fn extract(&mut self, event: &Value) -> io::Result<serde_json::Value> {
        if self.fail {
            return Err(io::Error::other("provider unavailable"));
        }
        Ok(serde_json::json!({"source":event["session"], "review":"proposed"}))
    }
}
#[test]
fn failure_preserves_queue_for_retry_and_success_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let capture = Queue::open(dir.path().join("capture")).unwrap();
    let event = serde_json::json!({"agent":"codex", "payload":{"session_id":"session"}});
    capture.reconcile(&event).unwrap();
    let id = capture.pending().unwrap()[0].0.clone();
    let output = dir.path().join("extractions");
    assert!(process(&capture, &output, &mut Provider { fail: true }).is_err());
    assert_eq!(capture.pending().unwrap().len(), 1);
    assert_eq!(
        process(&capture, &output, &mut Provider { fail: false }).unwrap(),
        1
    );
    assert!(capture.pending().unwrap().is_empty());
    let data: serde_json::Value =
        serde_json::from_slice(&std::fs::read(output.join(&id)).unwrap()).unwrap();
    assert_eq!(data["review"], "proposed");
    capture.reconcile(&event).unwrap();
    assert_eq!(
        process(&capture, &output, &mut Provider { fail: true }).unwrap(),
        0
    );
}
#[test]
fn durable_output_is_reused_after_interruption_before_ack() {
    let dir = tempfile::tempdir().unwrap();
    let capture = Queue::open(dir.path().join("capture")).unwrap();
    capture
        .reconcile(&serde_json::json!({"agent":"claude", "payload":{"session_id":"s"}}))
        .unwrap();
    let id = capture.pending().unwrap()[0].0.clone();
    let output = dir.path().join("extractions");
    std::fs::create_dir_all(&output).unwrap();
    std::fs::write(output.join(&id), b"{\"review\":\"proposed\"}").unwrap();
    assert_eq!(
        process(&capture, &output, &mut Provider { fail: true }).unwrap(),
        1
    );
    assert!(capture.pending().unwrap().is_empty());
}

#[test]
fn extractor_cannot_accept_a_decision() {
    struct Accepting;
    impl Extractor for Accepting {
        fn extract(&mut self, _: &Value) -> io::Result<serde_json::Value> {
            Ok(serde_json::json!({"decisions":[{"review":"accepted"}]}))
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let capture = Queue::open(dir.path().join("capture")).unwrap();
    capture
        .reconcile(&serde_json::json!({"agent":"codex", "payload":{"session_id":"s"}}))
        .unwrap();
    assert!(process(&capture, &dir.path().join("output"), &mut Accepting).is_err());
    assert_eq!(capture.pending().unwrap().len(), 1);
}

#[test]
fn integrated_extraction_retries_without_duplicate_proposals() {
    use doclify_domain::Store;
    use doclify_pipeline::process_proposals;
    struct Proposals {
        fail: bool,
    }
    impl Extractor for Proposals {
        fn extract(&mut self, event: &Value) -> io::Result<Value> {
            if self.fail {
                return Err(io::Error::other("offline"));
            }
            Ok(
                serde_json::json!({"proposals":[{"kind":"decision","summary":"Keep guest checkout","review":"proposed","evidence":[event["source_id"]]}]}),
            )
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let queue = Queue::open(dir.path().join("queue")).unwrap();
    queue
        .reconcile(&serde_json::json!({"agent":"codex","payload":{"session_id":"s"}}))
        .unwrap();
    let id = queue.pending().unwrap()[0].0.clone();
    let store = Store::open(dir.path()).unwrap();
    let output = dir.path().join("output");
    assert!(process_proposals(&queue, &output, &store, &mut Proposals { fail: true }).is_err());
    assert_eq!(queue.pending().unwrap().len(), 1);
    assert_eq!(
        process_proposals(&queue, &output, &store, &mut Proposals { fail: false }).unwrap(),
        1
    );
    let records = store.history(&format!("capture-{id}-0")).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].statement, "Keep guest checkout");
    assert_eq!(records[0].evidence, vec![format!("event:{id}")]);
    assert_eq!(records[0].review, doclify_domain::ReviewState::Proposed);
    queue
        .reconcile(&serde_json::json!({"agent":"codex","payload":{"session_id":"s"}}))
        .unwrap();
    assert_eq!(
        process_proposals(&queue, &output, &store, &mut Proposals { fail: true }).unwrap(),
        0
    );
}

#[test]
fn integrated_extraction_rejects_unknown_citations() {
    struct Invented;
    impl Extractor for Invented {
        fn extract(&mut self, _: &Value) -> io::Result<Value> {
            Ok(
                serde_json::json!({"proposals":[{"kind":"decision","summary":"x","review":"proposed","evidence":["event:invented"]}]}),
            )
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let queue = Queue::open(dir.path().join("queue")).unwrap();
    queue
        .reconcile(&serde_json::json!({"agent":"codex","payload":{"session_id":"s"}}))
        .unwrap();
    let store = doclify_domain::Store::open(dir.path()).unwrap();
    assert!(doclify_pipeline::process_proposals(
        &queue,
        &dir.path().join("out"),
        &store,
        &mut Invented
    )
    .is_err());
    assert_eq!(queue.pending().unwrap().len(), 1);
}

#[test]
fn persisted_output_recovers_after_import_before_ack() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("queue");
    let queue = Queue::open(&root).unwrap();
    queue
        .reconcile(&serde_json::json!({"agent":"codex","payload":{"session_id":"s"}}))
        .unwrap();
    let id = queue.pending().unwrap()[0].0.clone();
    let store = doclify_domain::Store::open(dir.path()).unwrap();
    let output = dir.path().join("out");
    struct One;
    impl Extractor for One {
        fn extract(&mut self, v: &Value) -> io::Result<Value> {
            Ok(
                serde_json::json!({"proposals":[{"kind":"task","summary":"Test it","review":"proposed","evidence":[v["source_id"]]}]}),
            )
        }
    }
    doclify_pipeline::process_proposals(&queue, &output, &store, &mut One).unwrap();
    std::fs::rename(root.join("done").join(&id), root.join("pending").join(&id)).unwrap();
    doclify_pipeline::process_proposals(&queue, &output, &store, &mut Provider { fail: true })
        .unwrap();
    assert_eq!(store.history(&format!("capture-{id}-0")).unwrap().len(), 1);
}

#[test]
fn discovery_only_reconciles_sessions_owned_by_repository() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    let other = dir.path().join("other");
    let sessions = dir.path().join("sessions");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&other).unwrap();
    std::fs::create_dir_all(&sessions).unwrap();
    for (name, cwd) in [("ours", &repo), ("theirs", &other)] {
        let line = serde_json::json!({"type":"session_meta","payload":{"id":name,"cwd":cwd}});
        std::fs::write(sessions.join(format!("{name}.jsonl")), format!("{line}\n")).unwrap();
    }
    let queue = Queue::open(dir.path().join("queue")).unwrap();
    assert_eq!(
        doclify_pipeline::discover_sessions(&queue, &repo, "codex", &sessions).unwrap(),
        1
    );
    assert!(queue
        .pending()
        .unwrap()
        .iter()
        .all(|(_, v)| !v.to_string().contains("theirs")));
    assert_eq!(
        doclify_pipeline::discover_sessions(&queue, &repo, "codex", &sessions).unwrap(),
        1
    );
}

#[test]
fn background_worker_processes_queued_events() {
    use std::time::Duration;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("queue");
    let hooks = dir.path().join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    Queue::open(&root)
        .unwrap()
        .reconcile(&serde_json::json!({"agent":"codex","payload":{"session_id":"s"}}))
        .unwrap();
    struct Empty;
    impl Extractor for Empty {
        fn extract(&mut self, _: &Value) -> io::Result<Value> {
            Ok(serde_json::json!({"proposals":[]}))
        }
    }
    let (worker, status) = doclify_pipeline::spawn_worker(
        root,
        hooks,
        dir.path().to_path_buf(),
        vec![],
        dir.path().join("output"),
        Empty,
        Duration::from_secs(60),
    );
    assert_eq!(
        status
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap(),
        1
    );
    worker.stop();
}

#[cfg(unix)]
#[test]
fn selected_cli_extracts_proposals_and_unavailable_codex_keeps_work() {
    use doclify_model_bridge::Bridge;
    use doclify_pipeline::{CliExtractor, SelectedExtractor};
    use std::{os::unix::fs::PermissionsExt, time::Duration};
    let dir = tempfile::tempdir().unwrap();
    let cli = dir.path().join("claude");
    std::fs::write(&cli, r##"#!/bin/sh
if [ "$1" = "--help" ]; then
 echo '--print --output-format --json-schema --tools --restricted --safe-mode --strict-mcp-config --no-session-persistence --disable-slash-commands --permission-mode --permission-prompts'
else
 [ "$DOCLIFY_BRIDGE" = 1 ] || exit 9
 cat >/dev/null
 echo '{"structured_output":{"proposals":[]}}'
fi
"##).unwrap();
    std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
    let queue = Queue::open(dir.path().join("queue")).unwrap();
    queue
        .reconcile(&serde_json::json!({"agent":"codex","payload":{"session_id":"s"}}))
        .unwrap();
    let store = doclify_domain::Store::open(dir.path()).unwrap();
    let out = dir.path().join("out");
    let mut selected = SelectedExtractor::Cli(CliExtractor {
        bridge: Bridge::codex(&cli),
        model: None,
        timeout: Duration::from_secs(2),
    });
    assert!(doclify_pipeline::process_proposals(&queue, &out, &store, &mut selected).is_err());
    assert_eq!(queue.pending().unwrap().len(), 1);
    selected = SelectedExtractor::Cli(CliExtractor {
        bridge: Bridge::claude(&cli),
        model: None,
        timeout: Duration::from_secs(2),
    });
    assert_eq!(
        doclify_pipeline::process_proposals(&queue, &out, &store, &mut selected).unwrap(),
        1
    );
    assert!(queue.pending().unwrap().is_empty());
}
