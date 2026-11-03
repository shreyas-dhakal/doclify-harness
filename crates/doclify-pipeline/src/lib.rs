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
    let _lock = capture.lock()?;
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

/// Synchronous constrained CLI adapter; run on a blocking worker.
pub struct CliExtractor {
    pub bridge: doclify_model_bridge::Bridge,
    pub model: Option<String>,
    pub timeout: std::time::Duration,
}
impl Extractor for CliExtractor {
    fn extract(&mut self, event: &Value) -> io::Result<Value> {
        use doclify_model_bridge::{providers, BridgeError, Request};
        let request = Request {
            prompt: format!("{}\n\n{}", providers::INSTRUCTION, event),
            schema: providers::proposal_schema(),
            model: self.model.clone(),
            timeout: self.timeout,
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let value =
            runtime
                .block_on(self.bridge.execute(request))
                .map_err(|error| match error {
                    BridgeError::Unsupported(reason) => {
                        io::Error::other(format!("CLI unavailable: {reason}"))
                    }
                    _ => io::Error::other("CLI extraction failed; captured work remains pending"),
                })?;
        providers::validate_proposals(&value.to_string()).map_err(io::Error::other)?;
        Ok(value)
    }
}

/// Explicit provider selection; failures never trigger an implicit fallback.
pub enum SelectedExtractor {
    Api(ApiExtractor),
    Cli(CliExtractor),
}
impl Extractor for SelectedExtractor {
    fn extract(&mut self, event: &Value) -> io::Result<Value> {
        match self {
            Self::Api(extractor) => extractor.extract(event),
            Self::Cli(extractor) => extractor.extract(event),
        }
    }
}

fn validate_extraction(
    batch: &doclify_model_bridge::providers::ProposalBatch,
    source: &str,
    event: &Value,
    store: &doclify_domain::Store,
) -> io::Result<()> {
    fn contains(value: &Value, excerpt: &str) -> bool {
        match value {
            Value::String(s) => s.contains(excerpt),
            Value::Array(a) => a.iter().any(|v| contains(v, excerpt)),
            Value::Object(o) => o.values().any(|v| contains(v, excerpt)),
            _ => false,
        }
    }
    let mut targets = std::collections::HashSet::new();
    for p in &batch.proposals {
        if p.evidence.iter().any(|e| e != source) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unknown source citation",
            ));
        }
        if p.excerpt
            .as_ref()
            .is_some_and(|e| e.trim().is_empty() || e.chars().count() > 500 || !contains(event, e))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "excerpt must be a bounded verbatim source excerpt",
            ));
        }
        if let Some(target) = &p.existing_record {
            if !targets.insert(&target.id) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "duplicate revision target",
                ));
            }
            let history = store.history(&target.id)?;
            let base = history
                .iter()
                .find(|r| r.version == target.version)
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "unknown record version")
                })?;
            let kind = serde_json::to_value(&base.kind).map_err(io::Error::other)?;
            if kind.as_str() != Some(&p.kind)
                || base.review == doclify_domain::ReviewState::Proposed
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid revision target",
                ));
            }
            let replay = history.get(target.version as usize).is_some_and(|r| {
                r.review == doclify_domain::ReviewState::Proposed
                    && r.provenance.iter().any(|p| p.source == source)
            });
            if !replay && history.last().map(|r| r.version) != Some(target.version) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "stale record version",
                ));
            }
        }
    }
    Ok(())
}

/// Extract, validate source references, and import immutable proposed records before acknowledgement.
/// Serialize this operation for each repository, including across processes.
pub fn process_proposals(
    capture: &Queue,
    output: &Path,
    store: &doclify_domain::Store,
    extractor: &mut impl Extractor,
) -> io::Result<usize> {
    let _lock = capture.lock()?;
    process_proposals_locked(capture, output, store, extractor)
}

/// Caller must hold the queue lock for the entire processing iteration.
pub fn process_proposals_locked(
    capture: &Queue,
    output: &Path,
    store: &doclify_domain::Store,
    extractor: &mut impl Extractor,
) -> io::Result<usize> {
    use doclify_domain::{
        ExperimentOutcome, ImplementationState, Provenance, Record, RecordKind, ReviewState,
    };
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
                extractor.extract(&serde_json::json!({"source_id":source,"event":event,"existing_records":store.latest_records()?}))?;
            // Validate before persisting so an invalid provider response can be retried.
            let batch = validate_proposals(&result.to_string()).map_err(io::Error::other)?;
            validate_extraction(&batch, &source, &event, store)?;
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
        validate_extraction(&batch, &source, &event, store)?;
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
            let kind = match p.kind.as_str() {
                "requirement" => RecordKind::Requirement,
                "decision" => RecordKind::Decision,
                "experiment" => RecordKind::Experiment,
                "question" => RecordKind::Question,
                "task" => RecordKind::Task,
                _ => unreachable!(),
            };
            let record_id = p
                .existing_record
                .as_ref()
                .map(|r| r.id.clone())
                .unwrap_or_else(|| format!("capture-{id}-{index}"));
            let history = store.history(&record_id)?;
            let (version, supersedes) = if let Some(target) = &p.existing_record {
                let base = history
                    .iter()
                    .find(|r| r.version == target.version)
                    .ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidData, "unknown record version")
                    })?;
                if base.kind != kind || base.review == ReviewState::Proposed {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "invalid revision target",
                    ));
                }
                // A retry may find the already-imported proposal after a crash.
                let replay = history.get(target.version as usize).is_some_and(|r| {
                    r.review == ReviewState::Proposed
                        && r.provenance.iter().any(|p| p.source == source)
                });
                if !replay && history.last().map(|r| r.version) != Some(target.version) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "stale record version",
                    ));
                }
                let mut accepted = None;
                for item in history.iter().take(target.version as usize) {
                    match item.review {
                        ReviewState::Accepted => accepted = Some(item.version),
                        ReviewState::Superseded if accepted == item.supersedes => accepted = None,
                        _ => (),
                    }
                }
                (target.version + 1, accepted)
            } else {
                (1, None)
            };
            let record = Record {
                id: record_id,
                version,
                kind,
                title: p.summary.clone(),
                statement: p.summary,
                rationale: p.rationale.unwrap_or_default(),
                review: ReviewState::Proposed,
                implementation: match p.implementation.as_deref() {
                    Some("planned") => ImplementationState::Planned,
                    Some("partial") => ImplementationState::Partial,
                    Some("implemented") => ImplementationState::Implemented,
                    Some("reverted") => ImplementationState::Reverted,
                    _ => ImplementationState::Unknown,
                },
                experiment_outcome: match p.experiment_outcome.as_deref() {
                    Some("active") => Some(ExperimentOutcome::Active),
                    Some("completed") => Some(ExperimentOutcome::Completed),
                    Some("abandoned") => Some(ExperimentOutcome::Abandoned),
                    _ => None,
                },
                provenance: vec![Provenance {
                    source: source.clone(),
                    excerpt: p.excerpt.or_else(|| {
                        event["text"]
                            .as_str()
                            .or(event["event"]["payload"]["prompt"].as_str())
                            .map(|s| s.chars().take(500).collect())
                    }),
                }],
                evidence: p.evidence,
                supersedes,
                scope: capture_scope(&event),
                session: capture_session(&event),
            };
            if history.len() < record.version as usize {
                store.propose(&record)?;
            } else if history.get(record.version as usize - 1) != Some(&record) {
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

fn capture_scope(event: &Value) -> Option<doclify_domain::RecordScope> {
    let origin = if event["source"] == "transcript" {
        &event["context"]
    } else {
        &event["event"]
    };
    let branch = origin["branch"].as_str().map(str::to_owned);
    let paths: Vec<String> = origin["payload"]["paths"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str())
                .filter(|p| {
                    !p.is_empty() && !p.starts_with('/') && !p.split('/').any(|part| part == "..")
                })
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    if branch.is_none() && paths.is_empty() {
        None
    } else {
        Some(doclify_domain::RecordScope { branch, paths })
    }
}

fn capture_session(event: &Value) -> Option<doclify_domain::SessionAttribution> {
    let origin = if event["source"] == "transcript" {
        &event["context"]
    } else {
        &event["event"]
    };
    let payload = &origin["payload"];
    let session_id = event["session_id"]
        .as_str()
        .or_else(|| payload["session_id"].as_str())
        .map(str::to_owned);
    let parent_session_id = event["raw"]["parent_session_id"]
        .as_str()
        .or_else(|| payload["parent_session_id"].as_str())
        .map(str::to_owned);
    if session_id.is_none() && parent_session_id.is_none() {
        None
    } else {
        Some(doclify_domain::SessionAttribution {
            worktree: origin["worktree"].as_str().map(str::to_owned),
            session_id,
            parent_session_id,
        })
    }
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
            queue.reconcile_snapshot(&serde_json::json!({"agent": agent, "worktree":repository, "branch":metadata["git"]["branch"], "payload": {"session_id":session, "parent_session_id":metadata["parent_session_id"], "transcript_path":entry.path()}}), &[root], Some(&bytes))?;
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
    reconcile_repository_locked(queue, hook_dir, repository, sessions)?;
    process_proposals_locked(queue, output, store, extractor)
}

/// Reconcile hooks and discover sessions while holding the queue lock.
pub fn reconcile_repository_locked(
    queue: &Queue,
    hook_dir: &Path,
    repository: &Path,
    sessions: &[(&str, &Path)],
) -> io::Result<usize> {
    let repository = repository.canonicalize()?;
    let mut paths = fs::read_dir(hook_dir)?
        .map(|e| e.map(|e| e.path()))
        .collect::<io::Result<Vec<_>>>()?;
    paths.retain(|p| p.extension().is_some_and(|ext| ext == "json"));
    paths.sort();
    let mut drained = 0;
    for path in paths {
        let result = (|| -> io::Result<()> {
            let event: Value = serde_json::from_slice(&fs::read(&path)?)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            if let Some(owner) = event["worktree"].as_str() {
                if Path::new(owner).canonicalize().ok().as_deref() != Some(repository.as_path()) {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "hook belongs to another worktree",
                    ));
                }
            }
            if let Some(transcript) = event["payload"]["transcript_path"].as_str() {
                let agent = event["agent"].as_str().unwrap_or("");
                let session = event["payload"]["session_id"].as_str().unwrap_or("");
                let path_obj = Path::new(transcript);
                let (root, relative) = sessions
                    .iter()
                    .filter(|(name, _)| *name == agent)
                    .find_map(|(_, root)| path_obj.strip_prefix(root).ok().map(|rel| (*root, rel)))
                    .ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::PermissionDenied,
                            "transcript outside agent roots",
                        )
                    })?;
                let dir = cap_std::fs::Dir::open_ambient_dir(root, cap_std::ambient_authority())?;
                let bytes = dir.read(relative)?;
                let first = bytes.split(|b| *b == b'\n').next().unwrap_or_default();
                let value: Value = serde_json::from_slice(first).map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "invalid transcript metadata",
                    )
                })?;
                let metadata = match agent {
                    "codex" if value["type"] == "session_meta" => &value["payload"],
                    "claude" => &value,
                    _ => {
                        return Err(io::Error::new(
                            io::ErrorKind::PermissionDenied,
                            "invalid transcript agent",
                        ))
                    }
                };
                let id = metadata[if agent == "codex" { "id" } else { "sessionId" }].as_str();
                let owns = metadata["cwd"]
                    .as_str()
                    .and_then(|cwd| Path::new(cwd).canonicalize().ok())
                    .is_some_and(|cwd| cwd.starts_with(&repository));
                if session.is_empty() || id != Some(session) || !owns {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "transcript session does not belong to repository",
                    ));
                }
                queue.reconcile_snapshot(&event, &[root], Some(&bytes))?;
            } else {
                queue.reconcile(&event)?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                fs::remove_file(&path)?;
                drained += 1;
            }
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::InvalidData
                        | io::ErrorKind::PermissionDenied
                        | io::ErrorKind::NotFound
                ) =>
            {
                let rejected = hook_dir.join("rejected");
                fs::create_dir_all(&rejected)?;
                let original = fs::read(&path)?;
                doclify_capture::queue::atomic_write(
                    &rejected.join(path.file_name().unwrap()),
                    &serde_json::to_vec(
                        &serde_json::json!({"event_bytes":original,"reason":e.to_string()}),
                    )?,
                )?;
                fs::remove_file(&path)?;
                fs::File::open(&rejected)?.sync_all()?;
            }
            Err(e) => return Err(e),
        }
        fs::File::open(hook_dir)?.sync_all()?;
    }
    for (agent, root) in sessions {
        discover_sessions(queue, &repository, agent, root)?;
    }
    Ok(drained)
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
/// Workers operating on the same queue serialize through its shared lock.
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

#[cfg(test)]
mod scope_metadata_tests {
    use super::*;
    #[test]
    fn transcript_metadata_preserves_child_and_origin_scope() {
        let event = serde_json::json!({"source":"transcript", "session_id":"child", "raw":{}, "context":{"worktree":"/repo/worktree-a", "branch":"feature/a", "payload":{"parent_session_id":"parent", "paths":["src/auth", "../escape"]}}});
        let scope = capture_scope(&event).unwrap();
        assert_eq!(scope.branch.as_deref(), Some("feature/a"));
        assert_eq!(scope.paths, ["src/auth"]);
        let session = capture_session(&event).unwrap();
        assert_eq!(session.worktree.as_deref(), Some("/repo/worktree-a"));
        assert_eq!(session.session_id.as_deref(), Some("child"));
        assert_eq!(session.parent_session_id.as_deref(), Some("parent"));
    }
}
