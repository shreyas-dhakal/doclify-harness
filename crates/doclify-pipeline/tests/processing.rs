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
