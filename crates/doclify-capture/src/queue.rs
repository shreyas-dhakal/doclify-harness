//! Local durable jobs, hook queue draining, and transcript checkpoints.
//! Workers can lock the queue across a complete iteration.
use crate::{hook, transcript, Agent};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
/// Sessions last written this long before the repository's `.git` was created
/// belong to an earlier repository at the same path, not this one.
const PREDATES_REPOSITORY: std::time::Duration = std::time::Duration::from_secs(3600);
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

pub struct Queue {
    root: PathBuf,
}
impl Queue {
    pub fn open(root: impl Into<PathBuf>) -> io::Result<Self> {
        let root = root.into();
        for dir in ["pending", "done", "checkpoints", "events", "unsupported"] {
            fs::create_dir_all(root.join(dir))?;
        }
        Ok(Self { root })
    }
    fn enqueue(&self, id: &str, value: &Value) -> io::Result<()> {
        if !self.root.join("done").join(id).exists() && !self.root.join("pending").join(id).exists()
        {
            atomic_write(
                &self.root.join("pending").join(id),
                &serde_json::to_vec(value)?,
            )?;
        }
        Ok(())
    }
    /// Reconcile one hook event. Lines the adapter does not understand are
    /// skipped and reported in `unsupported()` with the event needed to replay
    /// them; the supported entries around them are still queued.
    pub fn reconcile(&self, event: &Value) -> io::Result<()> {
        self.reconcile_with_roots(event, &[])
    }
    pub fn reconcile_with_roots(&self, event: &Value, roots: &[&Path]) -> io::Result<()> {
        self.reconcile_snapshot(event, roots, None)
    }
    pub fn reconcile_snapshot(
        &self,
        event: &Value,
        roots: &[&Path],
        snapshot: Option<&[u8]>,
    ) -> io::Result<()> {
        let agent_name = event["agent"]
            .as_str()
            .ok_or_else(|| invalid("missing agent"))?;
        let agent = Agent::parse(agent_name).ok_or_else(|| invalid("unsupported agent"))?;
        let raw = serde_json::to_vec(event)?;
        let event_id = digest(&raw);
        let payload = &event["payload"];
        if !payload.is_object()
            || (payload.get("transcript_path").is_some() && !payload["transcript_path"].is_string())
        {
            return Err(invalid("invalid hook payload"));
        }
        let Some(path) = payload["transcript_path"].as_str() else {
            atomic_write(&self.root.join("events").join(&event_id), &raw)?;
            if matches!(
                payload["hook_event_name"].as_str(),
                Some("PostToolUse" | "PostToolUseFailure")
            ) {
                return Ok(());
            }
            return self.enqueue(&event_id, &json!({"source":"hook", "event":event}));
        };
        let session = payload["session_id"]
            .as_str()
            .ok_or_else(|| invalid("transcript event missing session_id"))?;
        let path_obj = Path::new(path);
        let mut authorized = None;
        for root in roots {
            if let Ok(relative) = path_obj.strip_prefix(root) {
                let dir = cap_std::fs::Dir::open_ambient_dir(root, cap_std::ambient_authority())?;
                match dir.read(relative) {
                    Ok(bytes) => {
                        if snapshot.is_some_and(|s| s != bytes) {
                            return Err(invalid("transcript changed during discovery"));
                        }
                        let modified = dir
                            .metadata(relative)
                            .and_then(|m| m.modified())
                            .map(|t| t.into_std());
                        if modified.is_ok_and(|t| predates_repository(event, t)) {
                            return Ok(());
                        }
                        authorized = Some(bytes);
                        break;
                    }
                    Err(e) if e.kind() == io::ErrorKind::PermissionDenied => return Err(e),
                    Err(e) => return Err(e),
                }
            }
        }
        let bytes = authorized.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "transcript outside authorized roots",
            )
        })?;
        let key = digest(format!("{agent_name}\0{session}\0{path}").as_bytes());
        let cp_path = self.root.join("checkpoints").join(&key);
        let checkpoint: Option<Value> = read_json(&cp_path)?;
        let report_path = self.root.join("unsupported").join(&key);
        let report: Option<Value> = read_json(&report_path)?;
        let format = match agent {
            Agent::Codex => transcript::CODEX_FORMAT,
            Agent::Claude => transcript::CLAUDE_FORMAT,
        };
        // A report from another adapter version means skipped lines may now parse.
        let reread = report.as_ref().is_some_and(|r| r["format"] != format);
        let offset = checkpoint
            .as_ref()
            .filter(|_| !reread)
            .and_then(|c| {
                let n = c["offset"].as_u64()? as usize;
                (n <= bytes.len()
                    && c["prefix_hash"].as_str() == Some(digest(&bytes[..n]).as_str()))
                .then_some(n as u64)
            })
            .unwrap_or(0);
        let read = transcript::read_from(agent, std::io::Cursor::new(&bytes), offset)?;
        if !read.unsupported.is_empty() {
            let mut lines: Vec<Value> = report
                .filter(|_| offset > 0)
                .and_then(|r| r["lines"].as_array().cloned())
                .unwrap_or_default();
            lines.extend(read.unsupported.iter().map(|l| json!(l)));
            let listed: Vec<&str> = lines.iter().filter_map(Value::as_str).collect();
            let reason = format!(
                "{} transcript line(s) in {path} not understood by adapter {format}: {}. Supported entries were captured; these lines were skipped. Upgrade Doclify, then sync or replay unsupported transcripts to recover them (older-adapter reports are re-read automatically).",
                listed.len(),
                listed.join(", ")
            );
            atomic_write(
                &report_path,
                &serde_json::to_vec(
                    &json!({"agent":agent_name, "session_id":session, "transcript_path":path, "format":format, "lines":lines, "reason":reason, "event":event}),
                )?,
            )?;
        } else if offset == 0 && report_path.exists() {
            fs::remove_file(&report_path)?;
        }
        // Subagent rollouts name their parent only in `session_meta`, so the
        // checkpoint keeps it for entries read after that line.
        let explicit_parent = payload["parent_session_id"].as_str().map(str::to_owned);
        let parent = explicit_parent
            .clone()
            .or_else(|| {
                read.entries
                    .iter()
                    .filter(|e| e.kind == transcript::Kind::SessionMeta)
                    .find_map(|e| codex_parent(&e.raw))
            })
            .or_else(|| {
                checkpoint
                    .filter(|_| offset > 0)
                    .and_then(|c| c["parent_session_id"].as_str().map(str::to_owned))
            });
        atomic_write(&self.root.join("events").join(&event_id), &raw)?;
        // The transcript is the authoritative source for transcript-backed hooks.
        // Keep the raw hook for audit without extracting the same prompt twice:
        // each job carries only the hook's metadata, never its prompt or reply,
        // or every entry of the turn would show the model the same content.
        let mut context = event.clone();
        if let Some(p) = context["payload"].as_object_mut() {
            p.retain(|k, _| {
                matches!(
                    k.as_str(),
                    "session_id"
                        | "parent_session_id"
                        | "turn_id"
                        | "hook_event_name"
                        | "cwd"
                        | "transcript_path"
                        | "scope"
                        | "paths"
                )
            });
        }
        for mut entry in read.entries {
            let identity = entry.id.clone().unwrap_or_else(|| {
                format!(
                    "offset:{}:{}",
                    entry.offset,
                    digest(entry.raw.to_string().as_bytes())
                )
            });
            // Doclify's startup context, or the agent restating it, would
            // otherwise come back as fresh proposals for accepted records.
            if matches!(
                entry.kind,
                transcript::Kind::User | transcript::Kind::Assistant
            ) {
                let text = doclify_context::strip_quoted(&entry.text);
                if text != entry.text {
                    if text.trim().is_empty() {
                        continue;
                    }
                    for pointer in ["/message/content", "/payload/content"] {
                        if let Some(content) = entry.raw.pointer_mut(pointer) {
                            *content = json!(text);
                        }
                    }
                    entry.text = text;
                }
            }
            // Claude subagent entries live in their own sidechain transcript
            // under the parent's session id.
            let (session, parent) = match (&entry.raw["isSidechain"], entry.raw["agentId"].as_str())
            {
                (Value::Bool(true), Some(child)) => (
                    child,
                    explicit_parent.clone().or_else(|| {
                        Some(
                            entry.raw["sessionId"]
                                .as_str()
                                .unwrap_or(session)
                                .to_owned(),
                        )
                    }),
                ),
                _ => (session, parent.clone()),
            };
            let mut context = context.clone();
            if let (Some(p), Some(parent)) = (context["payload"].as_object_mut(), parent) {
                p.insert("parent_session_id".into(), json!(parent));
            }
            let id = digest(format!("{agent_name}\0{session}\0{identity}").as_bytes());
            self.enqueue(&id, &json!({"source":"transcript", "agent":agent_name, "session_id":session, "transcript_path":path, "offset":entry.offset, "kind":format!("{:?}", entry.kind), "text":entry.text, "raw":entry.raw, "context":context}))?;
        }
        let n = read.next_offset as usize;
        if n > bytes.len() {
            return Err(invalid("transcript changed while reading"));
        }
        atomic_write(
            &cp_path,
            &serde_json::to_vec(
                &json!({"offset":n, "prefix_hash":digest(&bytes[..n]), "parent_session_id":parent}),
            )?,
        )
    }
    /// Sessions with lines the adapter skipped: `{agent, session_id,
    /// transcript_path, format, lines, reason, event}`.
    pub fn unsupported(&self) -> io::Result<Vec<Value>> {
        let mut paths = fs::read_dir(self.root.join("unsupported"))?
            .map(|e| e.map(|v| v.path()))
            .collect::<io::Result<Vec<_>>>()?;
        paths.retain(|p| !p.file_name().unwrap().to_string_lossy().starts_with('.'));
        paths.sort();
        paths
            .into_iter()
            .map(|p| Ok(serde_json::from_slice(&fs::read(p)?)?))
            .collect()
    }
    /// Re-reads every reported session from the start using its stored event,
    /// so recovery works even after discovery stops finding the session. Ids
    /// keep already-queued entries from duplicating. Returns the reports
    /// cleared; a session that cannot be read keeps its report.
    pub fn replay_unsupported(&self, roots: &[&Path]) -> io::Result<usize> {
        let mut cleared = 0;
        for report in self.unsupported()? {
            let key = digest(
                format!(
                    "{}\0{}\0{}",
                    report["agent"].as_str().unwrap_or(""),
                    report["session_id"].as_str().unwrap_or(""),
                    report["transcript_path"].as_str().unwrap_or("")
                )
                .as_bytes(),
            );
            match fs::remove_file(self.root.join("checkpoints").join(&key)) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
            // ponytail: per-session read errors are left in the report, not surfaced.
            if self.reconcile_with_roots(&report["event"], roots).is_ok()
                && !self.root.join("unsupported").join(&key).exists()
            {
                cleared += 1;
            }
        }
        Ok(cleared)
    }
    /// Drain hook files only after their events and transcript jobs are durable.
    pub fn drain_hooks(&self, hook_dir: &Path) -> io::Result<usize> {
        self.drain_hooks_with_roots(hook_dir, &[])
    }
    pub fn drain_hooks_with_roots(&self, hook_dir: &Path, roots: &[&Path]) -> io::Result<usize> {
        let mut count = 0;
        for (path, event) in hook::pending(hook_dir)? {
            self.reconcile_with_roots(&event, roots)?;
            fs::remove_file(path)?;
            fs::File::open(hook_dir)?.sync_all()?;
            count += 1;
        }
        Ok(count)
    }
    pub fn lock(&self) -> io::Result<fs::File> {
        use fs2::FileExt;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.root.join(".worker.lock"))?;
        file.lock_exclusive()?;
        Ok(file)
    }
    pub fn pending(&self) -> io::Result<Vec<(String, Value)>> {
        let mut paths = fs::read_dir(self.root.join("pending"))?
            .map(|e| e.map(|v| v.path()))
            .collect::<io::Result<Vec<_>>>()?;
        paths.sort();
        paths
            .into_iter()
            .filter(|p| !p.file_name().unwrap().to_string_lossy().starts_with('.'))
            .map(|p| {
                let id = p.file_name().unwrap().to_string_lossy().into_owned();
                Ok((id, serde_json::from_slice(&fs::read(p)?)?))
            })
            .collect()
    }
    pub fn ack(&self, id: &str) -> io::Result<()> {
        if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(invalid("invalid job id"));
        }
        fs::rename(
            self.root.join("pending").join(id),
            self.root.join("done").join(id),
        )?;
        fs::File::open(self.root.join("pending"))?.sync_all()?;
        fs::File::open(self.root.join("done"))?.sync_all()
    }
}
fn read_json(path: &Path) -> io::Result<Option<Value>> {
    match fs::read(path) {
        Ok(b) => Ok(Some(serde_json::from_slice(&b)?)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Codex `session_meta` of a spawned subagent thread names its parent.
fn codex_parent(meta: &Value) -> Option<String> {
    let p = &meta["payload"];
    p["parent_session_id"]
        .as_str()
        .or_else(|| p["source"]["subagent"]["thread_spawn"]["parent_thread_id"].as_str())
        .map(str::to_owned)
}

/// Whether the transcript was last written well before the event's worktree
/// existed, i.e. it belongs to a deleted repository whose path was reused.
/// Unknown creation times (some filesystems) never exclude anything.
fn predates_repository(event: &Value, modified: std::time::SystemTime) -> bool {
    let Some(worktree) = event["worktree"].as_str() else {
        return false;
    };
    fs::metadata(Path::new(worktree).join(".git"))
        .and_then(|m| m.created())
        .is_ok_and(|created| modified + PREDATES_REPOSITORY < created)
}

pub fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| invalid("missing parent"))?;
    let temp = parent.join(format!(
        ".tmp-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        fs::File::open(parent)?.sync_all()
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}
