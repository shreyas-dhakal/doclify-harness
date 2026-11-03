//! Retryable extraction of durable capture events into local proposal payloads.
//! Consumers must validate proposals against their domain schema before applying them.
use doclify_capture::queue::{atomic_write, Queue};
use serde_json::Value;
use std::{fs, io, path::Path};

pub trait Extractor {
    fn extract(&mut self, event: &Value) -> io::Result<serde_json::Value>;
}

/// Process pending work serially. A provider or storage error stops processing and leaves
/// the current and remaining events pending. Persisted results are reused after a crash.
pub fn process(
    capture: &Queue,
    output: &Path,
    extractor: &mut impl Extractor,
) -> io::Result<usize> {
    fs::create_dir_all(output)?;
    let mut completed = 0;
    for (id, event) in capture.pending()? {
        let path = output.join(&id);
        if path.exists() {
            let value: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
            validate(&value)?;
        } else {
            let value = extractor.extract(&event)?;
            validate(&value)?;
            atomic_write(&path, &serde_json::to_vec(&value)?)?;
        }
        capture.ack(&id)?;
        completed += 1;
    }
    Ok(completed)
}
fn validate(value: &serde_json::Value) -> io::Result<()> {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(state) = map.get("review") {
                if state.as_str() != Some("proposed") {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "extractor cannot set a reviewed state",
                    ));
                }
            }
            for item in map.values() {
                validate(item)?;
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                validate(item)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Synchronous API provider adapter; run on a blocking worker.
pub struct ApiExtractor {
    pub provider: doclify_model_bridge::providers::ApiProvider,
    pub model: String,
}
impl Extractor for ApiExtractor {
    fn extract(&mut self, event: &Value) -> io::Result<Value> {
        let batch = self
            .provider
            .extract(&self.model, &event.to_string())
            .map_err(io::Error::other)?;
        serde_json::to_value(batch).map_err(io::Error::other)
    }
}

/// Extract, validate source references, and import immutable proposed records before acknowledgement.
/// Serialize this operation for each repository, including across processes.
pub fn process_proposals(
    capture: &Queue,
    output: &Path,
    store: &doclify_domain::Store,
    extractor: &mut impl Extractor,
) -> io::Result<usize> {
    use doclify_domain::{ImplementationState, Provenance, Record, RecordKind, ReviewState};
    use doclify_model_bridge::providers::validate_proposals;
    fs::create_dir_all(output)?;
    let mut count = 0;
    for (id, event) in capture.pending()? {
        let source = format!("event:{id}");
        let path = output.join(&id);
        let result = if path.exists() {
            serde_json::from_slice::<Value>(&fs::read(&path)?)?
        } else {
            let result =
                extractor.extract(&serde_json::json!({"source_id":source,"event":event}))?;
            // Validate before persisting so an invalid provider response can be retried.
            let batch = validate_proposals(&result.to_string()).map_err(io::Error::other)?;
            if batch
                .proposals
                .iter()
                .flat_map(|p| &p.evidence)
                .any(|e| e != &source)
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "unknown source citation",
                ));
            }
            atomic_write(&path, &serde_json::to_vec(&result)?)?;
            result
        };
        let batch = validate_proposals(&result.to_string()).map_err(io::Error::other)?;
        if batch
            .proposals
            .iter()
            .flat_map(|p| &p.evidence)
            .any(|e| e != &source)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unknown source citation",
            ));
        }
        for (index, p) in batch.proposals.into_iter().enumerate() {
            let record = Record {
                id: format!("capture-{id}-{index}"),
                version: 1,
                kind: match p.kind.as_str() {
                    "requirement" => RecordKind::Requirement,
                    "decision" => RecordKind::Decision,
                    "experiment" => RecordKind::Experiment,
                    "question" => RecordKind::Question,
                    "task" => RecordKind::Task,
                    _ => unreachable!(),
                },
                title: p.summary.clone(),
                statement: p.summary,
                rationale: String::new(),
                review: ReviewState::Proposed,
                implementation: ImplementationState::Unknown,
                experiment_outcome: None,
                provenance: vec![Provenance {
                    source: source.clone(),
                    excerpt: None,
                }],
                evidence: p.evidence,
                supersedes: None,
            };
            let history = store.history(&record.id)?;
            if history.is_empty() {
                store.propose(&record)?;
            } else if history.first() != Some(&record) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "existing proposal conflicts with extraction",
                ));
            }
        }
        capture.ack(&id)?;
        count += 1;
    }
    Ok(count)
}

/// Scan session trees without following symlinks. Only sessions whose first
/// metadata record identifies a working directory inside `repository` are used.
pub fn discover_sessions(
    queue: &Queue,
    repository: &Path,
    agent: &str,
    root: &Path,
) -> io::Result<usize> {
    if agent != "codex" && agent != "claude" {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unsupported agent",
        ));
    }
    let repository = repository.canonicalize()?;
    let scoped = cap_std::fs::Dir::open_ambient_dir(root, cap_std::ambient_authority())?;
    let mut dirs = vec![root.to_path_buf()];
    let mut count = 0;
    while let Some(dir) = dirs.pop() {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                dirs.push(entry.path());
                continue;
            }
            if !kind.is_file() || entry.path().extension().and_then(|s| s.to_str()) != Some("jsonl")
            {
                continue;
            }
            let path = entry.path();
            let relative = path.strip_prefix(root).map_err(io::Error::other)?;
            let bytes = scoped.read(relative)?;
            let Some(end) = bytes.iter().position(|b| *b == b'\n') else {
                continue;
            };
            let Ok(value) = serde_json::from_slice::<Value>(&bytes[..end]) else {
                continue;
            };
            let metadata = if agent == "codex" && value["type"] == "session_meta" {
                &value["payload"]
            } else if agent == "claude" {
                &value
            } else {
                continue;
            };
            let Some(cwd) = metadata["cwd"].as_str() else {
                continue;
            };
            let Some(session) =
                metadata[if agent == "codex" { "id" } else { "sessionId" }].as_str()
            else {
                continue;
            };
            if !Path::new(cwd)
                .canonicalize()
                .is_ok_and(|p| p.starts_with(&repository))
            {
                continue;
            }
            queue.reconcile_snapshot(&serde_json::json!({"agent": agent, "payload": {"session_id":session, "transcript_path":entry.path()}}), &[root], Some(&bytes))?;
            count += 1;
        }
    }
    Ok(count)
}

/// One serialized background iteration. Call periodically on a blocking thread;
/// errors leave work pending for a later iteration and must be reported by the caller.
pub fn run_once(
    queue: &Queue,
    hook_dir: &Path,
    repository: &Path,
    sessions: &[(&str, &Path)],
    output: &Path,
    store: &doclify_domain::Store,
    extractor: &mut impl Extractor,
) -> io::Result<usize> {
    let _lock = queue.lock()?;
    let roots: Vec<&Path> = sessions.iter().map(|(_, root)| *root).collect();
    queue.drain_hooks_with_roots(hook_dir, &roots)?;
    for (agent, root) in sessions {
        discover_sessions(queue, repository, agent, root)?;
    }
    process_proposals(queue, output, store, extractor)
}

/// Owns a background worker; stopping wakes it and waits for its current iteration.
pub struct Worker {
    stop: std::sync::mpsc::Sender<()>,
    thread: std::thread::JoinHandle<()>,
}
impl Worker {
    pub fn stop(self) {
        let _ = self.stop.send(());
        let _ = self.thread.join();
    }
}
/// Start a periodic serialized worker. The receiver reports every iteration's result;
/// the caller must ensure no other worker operates on this repository.
pub fn spawn_worker<E: Extractor + Send + 'static>(
    queue_root: std::path::PathBuf,
    hook_dir: std::path::PathBuf,
    repository: std::path::PathBuf,
    sessions: Vec<(String, std::path::PathBuf)>,
    output: std::path::PathBuf,
    mut extractor: E,
    interval: std::time::Duration,
) -> (Worker, std::sync::mpsc::Receiver<io::Result<usize>>) {
    let (stop, shutdown) = std::sync::mpsc::channel();
    let (report, status) = std::sync::mpsc::channel();
    let thread = std::thread::spawn(move || loop {
        let result = (|| {
            let queue = Queue::open(&queue_root)?;
            let store = doclify_domain::Store::open(&repository)?;
            let roots: Vec<_> = sessions
                .iter()
                .map(|(a, p)| (a.as_str(), p.as_path()))
                .collect();
            run_once(
                &queue,
                &hook_dir,
                &repository,
                &roots,
                &output,
                &store,
                &mut extractor,
            )
        })();
        if report.send(result).is_err() {
            break;
        }
        match shutdown.recv_timeout(interval) {
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => (),
            _ => break,
        }
    });
    (Worker { stop, thread }, status)
}
