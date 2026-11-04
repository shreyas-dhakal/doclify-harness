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
fn repository_reconciliation_reports_drained_events_and_discovered_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    let sessions = dir.path().join("sessions");
    let hooks = dir.path().join("hooks");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::create_dir_all(&hooks).unwrap();
    let meta = serde_json::json!({"type":"session_meta","payload":{"id":"s1","cwd":repo}});
    let user = serde_json::json!({"type":"response_item","payload":{"type":"message","id":"u1","role":"user","content":[{"type":"input_text","text":"requirement"}]}});
    std::fs::write(sessions.join("s1.jsonl"), format!("{meta}\n{user}\n")).unwrap();
    std::fs::write(
        hooks.join("event.json"),
        serde_json::to_vec(&serde_json::json!({"agent":"claude","payload":{"prompt":"decision"}}))
            .unwrap(),
    )
    .unwrap();
    let queue = Queue::open(dir.path().join("queue")).unwrap();
    let stats = doclify_pipeline::reconcile_repository_stats_locked(
        &queue,
        &hooks,
        &repo,
        &[("codex", sessions.as_path())],
    )
    .unwrap();
    assert_eq!(stats.drained_events, 1);
    assert_eq!(stats.discovered_sessions, 1);
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
 echo '--print --output-format --json-schema --verbose --tools --restricted --safe-mode --strict-mcp-config --no-session-persistence --disable-slash-commands --permission-mode --permission-prompts'
else
 [ "$DOCLIFY_BRIDGE" = 1 ] || exit 9
 cat >/dev/null
 echo '{"type":"system","subtype":"init","tools":["StructuredOutput"],"mcp_servers":[]}'
 echo '{"type":"result","structured_output":{"proposals":[]}}'
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

#[test]
fn hook_cannot_import_another_repositorys_transcript() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    let other = dir.path().join("other");
    let sessions = dir.path().join("sessions");
    let hooks = dir.path().join("hooks");
    for path in [&repo, &other, &sessions, &hooks] {
        std::fs::create_dir_all(path).unwrap();
    }
    let transcript = sessions.join("other.jsonl");
    std::fs::write(
        &transcript,
        format!(
            "{}\n",
            serde_json::json!({"type":"session_meta","payload":{"id":"other","cwd":other}})
        ),
    )
    .unwrap();
    std::fs::write(hooks.join("event.json"), serde_json::json!({"agent":"codex","payload":{"session_id":"other","transcript_path":transcript}}).to_string()).unwrap();
    let queue = Queue::open(dir.path().join("queue")).unwrap();
    let store = doclify_domain::Store::open(&repo).unwrap();
    let result = doclify_pipeline::run_once(
        &queue,
        &hooks,
        &repo,
        &[("codex", &sessions)],
        &dir.path().join("out"),
        &store,
        &mut Provider { fail: true },
    );
    assert_eq!(result.unwrap(), 0);
    assert!(hooks.join("rejected/event.json").exists());
    assert!(queue.pending().unwrap().is_empty());
}

#[test]
fn malformed_hook_does_not_block_later_valid_hook() {
    let dir = tempfile::tempdir().unwrap();
    let hooks = dir.path().join("hooks");
    std::fs::create_dir(&hooks).unwrap();
    std::fs::write(hooks.join("01.json"), b"{").unwrap();
    std::fs::write(hooks.join("02.json"), br#"{"agent":"codex","payload":{}}"#).unwrap();
    let queue = Queue::open(dir.path().join("queue")).unwrap();
    let store = doclify_domain::Store::open(dir.path()).unwrap();
    struct Empty;
    impl Extractor for Empty {
        fn extract(&mut self, _: &Value) -> io::Result<Value> {
            Ok(serde_json::json!({"proposals":[]}))
        }
    }
    assert_eq!(
        doclify_pipeline::run_once(
            &queue,
            &hooks,
            dir.path(),
            &[],
            &dir.path().join("out"),
            &store,
            &mut Empty
        )
        .unwrap(),
        1
    );
    assert!(hooks.join("rejected/01.json").exists());
}

#[test]
fn imported_proposal_preserves_supported_details() {
    struct Detailed;
    impl Extractor for Detailed {
        fn extract(&mut self, event: &Value) -> io::Result<Value> {
            Ok(
                serde_json::json!({"proposals":[{"kind":"decision","summary":"Allow guests","review":"proposed","evidence":[event["source_id"]],"rationale":"Avoid abandonment","excerpt":"Customers abandon account creation","implementation":"partial"}]}),
            )
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let queue = Queue::open(dir.path().join("queue")).unwrap();
    queue.reconcile(&serde_json::json!({"agent":"codex","payload":{"prompt":"Customers abandon account creation"}})).unwrap();
    let id = queue.pending().unwrap()[0].0.clone();
    let store = doclify_domain::Store::open(dir.path()).unwrap();
    doclify_pipeline::process_proposals(&queue, &dir.path().join("out"), &store, &mut Detailed)
        .unwrap();
    let record = &store.history(&format!("capture-{id}-0")).unwrap()[0];
    assert_eq!(record.rationale, "Avoid abandonment");
    assert_eq!(
        record.provenance[0].excerpt.as_deref(),
        Some("Customers abandon account creation")
    );
    assert_eq!(
        record.implementation,
        doclify_domain::ImplementationState::Partial
    );
}

#[test]
fn revisions_require_review_and_preserve_history_across_sessions() {
    use doclify_domain::{ImplementationState, ReviewState};
    struct Revision {
        target: Option<(String, u64)>,
        text: &'static str,
        implementation: &'static str,
    }
    impl Extractor for Revision {
        fn extract(&mut self, event: &Value) -> io::Result<Value> {
            if let Some((id, version)) = &self.target {
                assert!(event["existing_records"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|r| r["id"] == *id && r["version"] == *version));
            }
            Ok(
                serde_json::json!({"proposals":[{"kind":"decision","summary":self.text,"review":"proposed","evidence":[event["source_id"]],"existing_record":self.target.as_ref().map(|(id,version)| serde_json::json!({"id":id,"version":version})),"implementation":self.implementation}]}),
            )
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let queue = Queue::open(dir.path().join("queue")).unwrap();
    let store = doclify_domain::Store::open(dir.path()).unwrap();
    let out = dir.path().join("out");
    let enqueue = |session: &str| {
        queue
            .reconcile(&serde_json::json!({"agent":"codex","payload":{"session_id":session}}))
            .unwrap();
    };
    enqueue("one");
    let id = format!("capture-{}-0", queue.pending().unwrap()[0].0);
    doclify_pipeline::process_proposals(
        &queue,
        &out,
        &store,
        &mut Revision {
            target: None,
            text: "Allow guests",
            implementation: "planned",
        },
    )
    .unwrap();
    let first = store.review(&id, 1, ReviewState::Accepted).unwrap();
    enqueue("two");
    doclify_pipeline::process_proposals(
        &queue,
        &out,
        &store,
        &mut Revision {
            target: Some((id.clone(), 2)),
            text: "Require accounts",
            implementation: "implemented",
        },
    )
    .unwrap();
    assert_eq!(store.accepted(&id).unwrap(), Some(first.clone()));
    assert_eq!(store.history(&id).unwrap()[2].supersedes, Some(2));
    store.review(&id, 3, ReviewState::Rejected).unwrap();
    assert_eq!(store.accepted(&id).unwrap(), Some(first));
    enqueue("three");
    doclify_pipeline::process_proposals(
        &queue,
        &out,
        &store,
        &mut Revision {
            target: Some((id.clone(), 4)),
            text: "Require accounts",
            implementation: "implemented",
        },
    )
    .unwrap();
    let accepted = store.review(&id, 5, ReviewState::Accepted).unwrap();
    assert_eq!(store.accepted(&id).unwrap(), Some(accepted));
    enqueue("four");
    doclify_pipeline::process_proposals(
        &queue,
        &out,
        &store,
        &mut Revision {
            target: Some((id.clone(), 6)),
            text: "Allow guests again",
            implementation: "reverted",
        },
    )
    .unwrap();
    let reverted = store.review(&id, 7, ReviewState::Accepted).unwrap();
    assert_eq!(reverted.implementation, ImplementationState::Reverted);
    assert_eq!(store.history(&id).unwrap().len(), 8);
}

#[test]
fn unknown_revision_and_invented_excerpt_are_rejected_without_caching() {
    struct Bad {
        invented_excerpt: bool,
    }
    impl Extractor for Bad {
        fn extract(&mut self, event: &Value) -> io::Result<Value> {
            Ok(
                serde_json::json!({"proposals":[{"kind":"decision","summary":"Change","review":"proposed","evidence":[event["source_id"]],"existing_record":if self.invented_excerpt { None } else { Some(serde_json::json!({"id":"missing","version":1})) },"excerpt":if self.invented_excerpt { Some("invented words") } else { None }}]}),
            )
        }
    }
    for invented_excerpt in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let queue = Queue::open(dir.path().join("q")).unwrap();
        queue
            .reconcile(&serde_json::json!({"agent":"codex","payload":{"prompt":"actual words"}}))
            .unwrap();
        let store = doclify_domain::Store::open(dir.path()).unwrap();
        let out = dir.path().join("out");
        assert!(doclify_pipeline::process_proposals(
            &queue,
            &out,
            &store,
            &mut Bad { invented_excerpt }
        )
        .is_err());
        assert_eq!(std::fs::read_dir(out).unwrap().count(), 0);
        assert_eq!(queue.pending().unwrap().len(), 1);
    }
}

#[test]
fn extracted_experiment_outcome_is_preserved() {
    struct Experiment;
    impl Extractor for Experiment {
        fn extract(&mut self, event: &Value) -> io::Result<Value> {
            Ok(
                serde_json::json!({"proposals":[{"kind":"experiment","summary":"Try bypass","review":"proposed","evidence":[event["source_id"]],"experiment_outcome":"abandoned"}]}),
            )
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let queue = Queue::open(dir.path().join("q")).unwrap();
    queue
        .reconcile(&serde_json::json!({"agent":"claude","payload":{"prompt":"Abandon bypass"}}))
        .unwrap();
    let id = queue.pending().unwrap()[0].0.clone();
    let store = doclify_domain::Store::open(dir.path()).unwrap();
    doclify_pipeline::process_proposals(&queue, &dir.path().join("out"), &store, &mut Experiment)
        .unwrap();
    assert_eq!(
        store.history(&format!("capture-{id}-0")).unwrap()[0].experiment_outcome,
        Some(doclify_domain::ExperimentOutcome::Abandoned)
    );
}

#[test]
fn extracted_experiment_lifecycle_controls_directive_context() {
    struct Experiment(&'static str);
    impl Extractor for Experiment {
        fn extract(&mut self, event: &Value) -> io::Result<Value> {
            Ok(
                serde_json::json!({"proposals":[{"kind":"experiment","summary":format!("{} experiment", self.0),"review":"proposed","evidence":[event["source_id"]],"experiment_outcome":self.0}]}),
            )
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let queue = Queue::open(dir.path().join("q")).unwrap();
    let store = doclify_domain::Store::open(dir.path()).unwrap();
    let output = dir.path().join("out");
    for (outcome, prompt) in [
        ("active", "Start experiment"),
        ("completed", "Finish experiment"),
        ("abandoned", "Abandon experiment"),
    ] {
        queue
            .reconcile(&serde_json::json!({"agent":"codex","payload":{"prompt":prompt}}))
            .unwrap();
        let event_id = queue.pending().unwrap()[0].0.clone();
        doclify_pipeline::process_proposals(&queue, &output, &store, &mut Experiment(outcome))
            .unwrap();
        store
            .review(
                &format!("capture-{event_id}-0"),
                1,
                doclify_domain::ReviewState::Accepted,
            )
            .unwrap();
    }

    let context = doclify_context::build(
        dir.path(),
        &doclify_context::Scope::default(),
        &doclify_context::Limits::default(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(context.open_work.len(), 1);
    assert_eq!(
        context.open_work[0].status,
        "experiment, active; not a decision"
    );
    assert!(context.render().contains("active experiment"));
    assert!(!context
        .open_work
        .iter()
        .any(|item| item.title.contains("completed") || item.title.contains("abandoned")));
}

/// Claude Code 2.1.308 opens transcripts with cwd-less `queue-operation` lines;
/// ownership comes from the first line that carries a cwd.
#[test]
fn claude_transcript_with_leading_queue_operations_is_owned() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    let sessions = dir.path().join("sessions");
    let hooks = dir.path().join("hooks");
    for path in [&repo, &sessions, &hooks] {
        std::fs::create_dir_all(path).unwrap();
    }
    let transcript = sessions.join("s.jsonl");
    let lines = [
        serde_json::json!({"type":"queue-operation","operation":"enqueue","sessionId":"s"}),
        serde_json::json!({"type":"user","uuid":"u1","sessionId":"s","cwd":repo,"message":{"role":"user","content":"login work"}}),
    ];
    std::fs::write(&transcript, lines.map(|l| format!("{l}\n")).concat()).unwrap();
    std::fs::write(hooks.join("event.json"), serde_json::json!({"agent":"claude","payload":{"session_id":"s","transcript_path":transcript}}).to_string()).unwrap();
    let queue = Queue::open(dir.path().join("queue")).unwrap();
    let store = doclify_domain::Store::open(&repo).unwrap();
    doclify_pipeline::run_once(
        &queue,
        &hooks,
        &repo,
        &[("claude", &sessions)],
        &dir.path().join("out"),
        &store,
        &mut Provider { fail: true },
    )
    .ok();
    assert!(!hooks.join("rejected/event.json").exists());
    assert_eq!(queue.pending().unwrap().len(), 1);
    assert_eq!(
        doclify_pipeline::discover_sessions(&queue, &repo, "claude", &sessions).unwrap(),
        1
    );
}
