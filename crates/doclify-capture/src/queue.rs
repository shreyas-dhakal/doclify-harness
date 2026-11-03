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
        for dir in ["pending", "done", "checkpoints", "events"] {
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
    /// Reconcile one hook event. Unknown formats return an error without advancing
    /// the checkpoint. Raw events are kept locally, including on error.
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
            if matches!(payload["hook_event_name"].as_str(), Some("PostToolUse" | "PostToolUseFailure")) {
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
        let cp_path = self.root.join("checkpoints").join(key);
        let checkpoint: Option<Value> = match fs::read(&cp_path) {
            Ok(b) => Some(serde_json::from_slice(&b)?),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        };
        let offset = checkpoint
            .and_then(|c| {
                let n = c["offset"].as_u64()? as usize;
                (n <= bytes.len()
                    && c["prefix_hash"].as_str() == Some(digest(&bytes[..n]).as_str()))
                .then_some(n as u64)
            })
            .unwrap_or(0);
        let read = transcript::read_from(agent, std::io::Cursor::new(&bytes), offset)?;
        if !read.unsupported.is_empty() {
            return Err(invalid(format!(
                "unsupported transcript {path}: {}",
                read.unsupported.join(", ")
            )));
        }
        atomic_write(&self.root.join("events").join(&event_id), &raw)?;
        // The transcript is the authoritative source for transcript-backed hooks.
        // Keep the raw hook for audit without extracting the same prompt twice.
        for entry in read.entries {
            let identity = entry.id.clone().unwrap_or_else(|| {
                format!(
                    "offset:{}:{}",
                    entry.offset,
                    digest(entry.raw.to_string().as_bytes())
                )
            });
            let id = digest(format!("{agent_name}\0{session}\0{identity}").as_bytes());
            self.enqueue(&id, &json!({"source":"transcript", "agent":agent_name, "session_id":session, "transcript_path":path, "offset":entry.offset, "kind":format!("{:?}", entry.kind), "text":entry.text, "raw":entry.raw, "context":event}))?;
        }
        let n = read.next_offset as usize;
        if n > bytes.len() {
            return Err(invalid("transcript changed while reading"));
        }
        atomic_write(
            &cp_path,
            &serde_json::to_vec(&json!({"offset":n, "prefix_hash":digest(&bytes[..n])}))?,
        )
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
